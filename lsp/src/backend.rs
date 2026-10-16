use std::str::FromStr;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use tokio::sync::RwLock;
use tower_lsp_server::ls_types::request::{
    GotoDeclarationParams, GotoDeclarationResponse, GotoImplementationParams,
    GotoImplementationResponse, GotoTypeDefinitionParams, GotoTypeDefinitionResponse,
};
use tower_lsp_server::{Client, LanguageServer, jsonrpc::Result, ls_types::*};

use crate::{analysis, completion, diagnostics, folding, position, semantic, symbols, zora};

const DIAGNOSTIC_DEBOUNCE: Duration = Duration::from_millis(30);

#[derive(Debug)]
pub struct Backend {
    client: Client,
    documents: Arc<RwLock<HashMap<Uri, Document>>>,
    deps: Arc<RwLock<HashMap<PathBuf, CachedDeps>>>,
    symbols: Arc<RwLock<symbols::SymbolIndex>>,
    repair_cache: Arc<RwLock<HashMap<(u64, u64), analysis::Analysis>>>,
}

#[derive(Debug, Clone, Default)]
struct CachedDeps {
    packages: Vec<(String, PathBuf)>,
    toml_mtime: Option<std::time::SystemTime>,
    lock_mtime: Option<std::time::SystemTime>,
}

#[derive(Debug, Clone)]
struct Document {
    version: i32,
    text: String,
    analysis: analysis::Analysis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompletionBranch {
    StructInit,
    Dot(usize),
    Call(completion::CallTrigger),
}

async fn run_and_capture(
    mut command: tokio::process::Command,
    binary: std::path::PathBuf,
) -> std::result::Result<String, String> {
    let output = command.output().await.map_err(|err| {
        format!("zeen binary not found or failed to start: {err} (install it into PATH)")
    })?;

    let mut report = String::from_utf8_lossy(&output.stdout).into_owned();

    if !output.status.success() {
        report.push_str(&String::from_utf8_lossy(&output.stderr));
    }

    if report.len() > 1500 {
        report.truncate(1500);
        report.push_str("\n... truncated");
    }

    let _ = std::fs::remove_file(&binary);

    if output.status.success() {
        Ok(report)
    } else {
        Err(report)
    }
}

fn hierarchy_docs(documents: &HashMap<Uri, Document>) -> Vec<crate::hierarchy::HierarchyDoc<'_>> {
    documents
        .iter()
        .map(|(uri, document)| crate::hierarchy::HierarchyDoc {
            uri: uri.clone(),
            text: &document.text,
            analysis: &document.analysis,
        })
        .collect()
}

async fn packages_for_cache(
    deps: &Arc<RwLock<HashMap<PathBuf, CachedDeps>>>,
    root: Option<&std::path::Path>,
) -> (Vec<(String, PathBuf)>, Option<String>) {
    let Some(root) = root else {
        return (Vec::new(), None);
    };

    {
        let cached = deps.read().await;

        if let Some(entry) = cached.get(root) {
            let (toml, lock) = zora::manifest_mtimes(root);

            if entry.toml_mtime == toml && entry.lock_mtime == lock {
                return (entry.packages.clone(), None);
            }
        }
    }

    let root_buf = root.to_path_buf();
    let result = tokio::task::spawn_blocking(move || zora::run_deps_map(&root_buf)).await;

    match result {
        Ok(deps_map) => {
            let (toml, lock) = zora::manifest_mtimes(root);
            let mut packages = deps_map.packages;

            if let Some(self_package) = zora::self_mapping(root) {
                packages.push(self_package);
            }

            let output = (packages.clone(), deps_map.stderr);

            deps.write().await.insert(
                root.to_path_buf(),
                CachedDeps {
                    packages,
                    toml_mtime: toml,
                    lock_mtime: lock,
                },
            );

            output
        }
        Err(_) => {
            let (toml, lock) = zora::manifest_mtimes(root);
            let packages = zora::self_mapping(root)
                .map(|pair| vec![pair])
                .unwrap_or_default();
            let output = (packages.clone(), None);

            deps.write().await.insert(
                root.to_path_buf(),
                CachedDeps {
                    packages,
                    toml_mtime: toml,
                    lock_mtime: lock,
                },
            );

            output
        }
    }
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
            deps: Arc::new(RwLock::new(HashMap::new())),
            symbols: Arc::new(RwLock::new(symbols::SymbolIndex::default())),
            repair_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn packages_for(
        &self,
        root: Option<&std::path::Path>,
    ) -> (Vec<(String, PathBuf)>, Option<String>) {
        packages_for_cache(&self.deps, root).await
    }

    fn zora_root_of(&self, uri: &Uri) -> Option<PathBuf> {
        let path = diagnostics::file_uri_to_path(uri)?;
        zora::find_root(&path)
    }

    async fn resolve_target_text(&self, uri: &Uri, target_file: &str) -> Option<(Uri, String)> {
        if let Some(sibling) = diagnostics::file_uri_to_path(uri)?
            .parent()
            .map(|parent| parent.join(target_file))
            && sibling.is_file()
        {
            let sibling_uri = Uri::from_str(&format!("file://{}", sibling.display())).ok()?;

            if let Some(document) = self.documents.read().await.get(&sibling_uri) {
                return Some((sibling_uri, document.text.clone()));
            }

            if let Ok(text) = std::fs::read_to_string(&sibling) {
                return Some((sibling_uri, text));
            }
        }

        let path = std::path::PathBuf::from(target_file);

        if path.is_absolute() && path.is_file() {
            let target_uri = Uri::from_str(&format!("file://{}", path.display())).ok()?;

            if let Some(document) = self.documents.read().await.get(&target_uri) {
                return Some((target_uri, document.text.clone()));
            }

            if let Ok(text) = std::fs::read_to_string(&path) {
                return Some((target_uri, text));
            }
        }

        None
    }

    fn repair_cache_key(text: &str, packages: &[(String, PathBuf)]) -> (u64, u64) {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut text_hasher = DefaultHasher::new();
        text.hash(&mut text_hasher);

        let mut packages_hasher = DefaultHasher::new();

        for (name, dir) in packages {
            name.hash(&mut packages_hasher);
            dir.hash(&mut packages_hasher);
        }

        (text_hasher.finish(), packages_hasher.finish())
    }

    async fn retry_with_repaired(
        &self,
        uri: &Uri,
        text: &str,
        offset: usize,
        paths: completion::ResolvePaths<'_>,
        branch: CompletionBranch,
    ) -> Vec<CompletionItem> {
        let key = Self::repair_cache_key(text, paths.packages());

        if let Some(cached) = self.repair_cache.read().await.get(&key) {
            return match branch {
                CompletionBranch::StructInit => {
                    completion::struct_init_complete(text, cached, offset, paths)
                }
                CompletionBranch::Dot(dot) => completion::dot_complete(text, cached, dot, paths),
                CompletionBranch::Call(trigger) => {
                    completion::call_complete(text, cached, trigger, paths)
                }
            };
        }

        let mut output = diagnostics::CheckOutput {
            diagnostics: Vec::new(),
            analysis: crate::analysis::Analysis::default(),
        };
        let mut seen = String::new();

        for extra in 0..=3 {
            for semi in [false, true] {
                if !output.analysis.occurrences.is_empty() {
                    break;
                }

                let candidate = if extra == 0 && !semi {
                    completion::repair_for_completion(text, offset)
                } else {
                    completion::repair_for_completion_semi(text, offset, extra, semi)
                };

                if let Some(next) = candidate
                    && next != seen
                    && diagnostics::parses(&next)
                {
                    seen = next.clone();
                    let retry = diagnostics::check_completion(uri, &next, paths.packages());

                    if !retry.analysis.occurrences.is_empty() {
                        output = retry;
                    }
                }
            }
        }

        if output.analysis.occurrences.is_empty() {
            return Vec::new();
        }

        let mut cache = self.repair_cache.write().await;

        if cache.len() > 16 {
            cache.clear();
        }

        cache.insert(key, output.analysis.clone());

        match branch {
            CompletionBranch::StructInit => {
                completion::struct_init_complete(text, &output.analysis, offset, paths)
            }
            CompletionBranch::Dot(dot) => {
                completion::dot_complete(text, &output.analysis, dot, paths)
            }
            CompletionBranch::Call(trigger) => {
                completion::call_complete(text, &output.analysis, trigger, paths)
            }
        }
    }

    async fn run_main(&self, arguments: Vec<LSPAny>) {
        let Some(uri) = arguments.first().and_then(|value| value.as_str()) else {
            return;
        };

        let Ok(uri) = Uri::from_str(uri) else {
            return;
        };

        let documents = self.documents.read().await;

        let Some(document) = documents.get(&uri) else {
            return;
        };

        let Some(open_path) = diagnostics::file_uri_to_path(&uri) else {
            return;
        };

        let context = diagnostics::entry_context(&open_path, &document.text);

        if context.entry != open_path {
            self.client
                .show_message(
                    MessageType::WARNING,
                    "run is only available from the entry file",
                )
                .await;
            return;
        }

        if std::fs::read_to_string(&context.entry).ok().as_deref() != Some(document.text.as_str()) {
            self.client
                .show_message(MessageType::WARNING, "save the file first")
                .await;
            return;
        }

        let zora_root = context.zora.clone();
        let (packages, _) = self.packages_for(zora_root.as_deref()).await;
        let binary = std::env::temp_dir().join(format!("zeen-lsp-run-{}", std::process::id()));

        #[cfg(windows)]
        let binary = binary.with_extension("exe");

        let mut command = tokio::process::Command::new("zeen");
        command
            .arg(&context.entry)
            .arg(&binary)
            .current_dir(&context.root);

        for (name, dir) in &packages {
            command
                .arg("--pkg")
                .arg(format!("{name}={}", dir.display()));
        }

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            run_and_capture(command, binary.clone()),
        )
        .await;

        match output {
            Err(_) => {
                self.client
                    .show_message(MessageType::ERROR, "run timed out after 120s")
                    .await;
            }
            Ok(Err(message)) => {
                self.client.show_message(MessageType::ERROR, message).await;
            }
            Ok(Ok(report)) => {
                self.client.show_message(MessageType::INFO, report).await;
            }
        }
    }

    async fn show_references(&self, arguments: Vec<LSPAny>) {
        let (Some(uri), Some(offset)) = (
            arguments.first().and_then(|value| value.as_str()),
            arguments.get(1).and_then(|value| value.as_u64()),
        ) else {
            return;
        };

        let Ok(uri) = Uri::from_str(uri) else {
            return;
        };

        let documents = self.documents.read().await;
        let docs: Vec<crate::rename::RenameDoc<'_>> = documents
            .iter()
            .map(|(uri, document)| crate::rename::RenameDoc {
                uri: uri.clone(),
                text: &document.text,
                analysis: &document.analysis,
            })
            .collect();

        let spans = crate::rename::reference_spans(&docs, &uri, offset as usize);

        if spans.is_empty() {
            self.client
                .show_message(MessageType::INFO, "no references found")
                .await;
            return;
        }

        let mut lines = Vec::with_capacity(spans.len().min(20));

        for (uri, range) in spans.iter().take(20) {
            lines.push(format!(
                "{}:{}",
                uri.as_str().rsplit('/').next().unwrap_or(uri.as_str()),
                range.start.line + 1,
            ));
        }

        if spans.len() > 20 {
            lines.push(format!("... and {} more", spans.len() - 20));
        }

        self.client
            .show_message(MessageType::INFO, lines.join("\n"))
            .await;
    }
    async fn jump_to_def(
        &self,
        uri: &Uri,
        document: &Document,
        def: zeen_resolve::DefId,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let Some((target_offset, target_len, target_file)) = document.analysis.def_spans.get(&def)
        else {
            return Ok(None);
        };

        let (target_uri, target_text) = match target_file {
            Some(target_name) => {
                let Some(resolved) = self.resolve_target_text(uri, target_name).await else {
                    return Ok(None);
                };
                resolved
            }
            None => (uri.clone(), document.text.clone()),
        };

        Ok(Some(GotoDefinitionResponse::Array(vec![Location {
            uri: target_uri,
            range: position::span_to_range(&target_text, *target_offset, *target_len),
        }])))
    }

    async fn dependent_documents(
        &self,
        changed: &[PathBuf],
    ) -> Vec<(Uri, String, i32, Vec<(String, PathBuf)>)> {
        let documents = self.documents.read().await;
        let mut out = Vec::new();

        for (uri, document) in documents.iter() {
            let Some(open_path) = diagnostics::file_uri_to_path(uri) else {
                continue;
            };

            let canonical = std::fs::canonicalize(&open_path).unwrap_or_else(|_| open_path.clone());

            let references = changed.iter().any(|source| {
                source == &canonical
                    || document
                        .analysis
                        .def_spans
                        .values()
                        .any(|(_, _, file)| file.as_deref() == source.to_str())
                    || document.analysis.occurrences.iter().any(|occurrence| {
                        occurrence.target_file.as_deref() == source.to_str()
                            || occurrence
                                .target_file
                                .as_deref()
                                .and_then(|name| open_path.parent().map(|parent| parent.join(name)))
                                .and_then(|sibling| std::fs::canonicalize(sibling).ok())
                                .is_some_and(|sibling| &sibling == source)
                    })
            });

            if !references {
                continue;
            }

            let root = zora::find_root(&open_path);
            let packages = self.packages_for(root.as_deref()).await.0;

            out.push((
                uri.clone(),
                document.text.clone(),
                document.version,
                packages,
            ));
        }

        out
    }
}

impl LanguageServer for Backend {
    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let documents = self.documents.read().await;

        let Some(document) = documents.get(&uri) else {
            return Ok(None);
        };

        let offset = position::position_to_offset(
            &document.text,
            params.text_document_position_params.position,
        );

        let Some(occurence) = document.analysis.at(offset) else {
            return Ok(None);
        };

        let key = (occurence.offset, occurence.len);
        if let Some(fn_def) = crate::hover::method_at(&document.analysis, key) {
            return self.jump_to_def(&uri, document, fn_def).await;
        }

        let (Some(target_offset), Some(target_len)) =
            (occurence.target_offset, occurence.target_len)
        else {
            return Ok(None);
        };

        let (target_uri, target_text) = match &occurence.target_file {
            Some(target_name) => {
                let Some(resolved) = self.resolve_target_text(&uri, target_name).await else {
                    return Ok(None);
                };
                resolved
            }
            None => (uri.clone(), document.text.clone()),
        };

        Ok(Some(GotoDefinitionResponse::Array(vec![Location {
            uri: target_uri,
            range: position::span_to_range(&target_text, target_offset, target_len),
        }])))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let documents = self.documents.read().await;

        let Some(document) = documents.get(&uri) else {
            return Ok(None);
        };

        let offset =
            position::position_to_offset(&document.text, params.text_document_position.position);
        let docs: Vec<crate::rename::RenameDoc<'_>> = documents
            .iter()
            .map(|(uri, document)| crate::rename::RenameDoc {
                uri: uri.clone(),
                text: &document.text,
                analysis: &document.analysis,
            })
            .collect();

        let edits = crate::rename::rename_edits(&docs, &uri, offset, &params.new_name);

        if edits.is_empty() {
            return Ok(None);
        }

        Ok(Some(WorkspaceEdit {
            changes: Some(edits),
            ..Default::default()
        }))
    }

    async fn goto_type_definition(
        &self,
        params: GotoTypeDefinitionParams,
    ) -> Result<Option<GotoTypeDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let documents = self.documents.read().await;

        let Some(document) = documents.get(&uri) else {
            return Ok(None);
        };

        let offset = position::position_to_offset(
            &document.text,
            params.text_document_position_params.position,
        );

        let Some(occurrence) = document.analysis.at(offset) else {
            return Ok(None);
        };

        let Some(def) = occurrence.type_def else {
            return Ok(None);
        };

        self.jump_to_def(&uri, document, def).await
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let documents = self.documents.read().await;

        let Some(document) = documents.get(&uri) else {
            return Ok(None);
        };

        let offset =
            position::position_to_offset(&document.text, params.text_document_position.position);
        let docs: Vec<crate::rename::RenameDoc<'_>> = documents
            .iter()
            .map(|(uri, document)| crate::rename::RenameDoc {
                uri: uri.clone(),
                text: &document.text,
                analysis: &document.analysis,
            })
            .collect();

        let spans = crate::rename::reference_spans(&docs, &uri, offset);

        if spans.is_empty() {
            return Ok(None);
        }

        Ok(Some(
            spans
                .into_iter()
                .map(|(uri, range)| Location { uri, range })
                .collect(),
        ))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let documents = self.documents.read().await;

        let Some(document) = documents.get(&uri) else {
            return Ok(None);
        };

        let offset = position::position_to_offset(
            &document.text,
            params.text_document_position_params.position,
        );
        let docs: Vec<crate::rename::RenameDoc<'_>> = documents
            .iter()
            .map(|(uri, document)| crate::rename::RenameDoc {
                uri: uri.clone(),
                text: &document.text,
                analysis: &document.analysis,
            })
            .collect();

        let highlights = crate::rename::highlight_spans(&docs, &uri, offset);

        if highlights.is_empty() {
            return Ok(None);
        }

        Ok(Some(
            highlights
                .into_iter()
                .map(|(range, write)| DocumentHighlight {
                    range,
                    kind: Some(if write {
                        DocumentHighlightKind::WRITE
                    } else {
                        DocumentHighlightKind::READ
                    }),
                })
                .collect(),
        ))
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        let documents = self.documents.read().await;

        let Some(document) = documents.get(&params.text_document.uri) else {
            return Ok(None);
        };

        let offset = position::position_to_offset(&document.text, params.position);
        let docs: Vec<crate::rename::RenameDoc<'_>> = documents
            .iter()
            .map(|(uri, document)| crate::rename::RenameDoc {
                uri: uri.clone(),
                text: &document.text,
                analysis: &document.analysis,
            })
            .collect();

        match crate::rename::rename_range(&docs, &params.text_document.uri, offset) {
            Some(range) => Ok(Some(PrepareRenameResponse::Range(range))),
            None => Err(tower_lsp_server::jsonrpc::Error::invalid_params(
                "nothing renameable here",
            )),
        }
    }

    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                semantic_tokens_provider: Some(
                    SemanticTokensOptions {
                        legend: semantic::legend(),
                        full: Some(SemanticTokensFullOptions::Bool(true)),
                        range: Some(true),
                        ..Default::default()
                    }
                    .into(),
                ),
                definition_provider: Some(OneOf::Left(true)),
                type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
                references_provider: Some(OneOf::Left(true)),
                document_highlight_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                rename_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                inlay_hint_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string(), "@".to_string()]),
                    ..Default::default()
                }),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    ..Default::default()
                }),
                code_lens_provider: Some(CodeLensOptions {
                    resolve_provider: None,
                }),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        crate::codelens::RUN_COMMAND.to_string(),
                        crate::codelens::REFERENCES_COMMAND.to_string(),
                    ],
                    ..Default::default()
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: env!("CARGO_PKG_NAME").to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "zeen-lsp ready")
            .await;

        let watchers = [zora::MANIFEST, zora::LOCKFILE, "*.zn"]
            .into_iter()
            .map(|file| FileSystemWatcher {
                glob_pattern: GlobPattern::String(format!("**/{file}")),
                kind: None,
            })
            .collect();

        let options = serde_json::to_value(DidChangeWatchedFilesRegistrationOptions { watchers });

        if let Ok(register_options) = options {
            let _ = self
                .client
                .register_capability(vec![Registration {
                    id: "zeen-zora-watch".to_string(),
                    method: "workspace/didChangeWatchedFiles".to_string(),
                    register_options: Some(register_options),
                }])
                .await;
        }
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let document = self
            .documents
            .read()
            .await
            .get(&params.text_document.uri)
            .cloned();

        let Some(document) = document else {
            return Ok(None);
        };

        let tokens = semantic::tokens_for(&document.text, &document.analysis);

        Ok(Some(
            SemanticTokens {
                result_id: None,
                data: tokens,
            }
            .into(),
        ))
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let document = self
            .documents
            .read()
            .await
            .get(&params.text_document.uri)
            .cloned();

        let Some(document) = document else {
            return Ok(None);
        };

        Ok(Some(
            symbols::document_symbols(&document.text, &document.analysis).into(),
        ))
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<WorkspaceSymbolResponse>> {
        if params.query.is_empty() {
            return Ok(Some(WorkspaceSymbolResponse::Flat(Vec::new())));
        }

        let mut roots = HashSet::new();

        for uri in self.documents.read().await.keys() {
            let Some(path) = diagnostics::file_uri_to_path(uri) else {
                continue;
            };

            if let Some(root) = zora::find_root(&path) {
                roots.insert(root);
            } else if let Some(parent) = path.parent() {
                roots.insert(parent.to_path_buf());
            }
        }

        for entry in self.deps.read().await.values() {
            for (_, dir) in &entry.packages {
                roots.insert(dir.clone());
            }
        }

        let roots: Vec<PathBuf> = roots.into_iter().collect();
        let mut index = self.symbols.write().await;
        index.refresh(&roots);

        Ok(Some(WorkspaceSymbolResponse::Flat(
            symbols::workspace_symbols(&index, &params.query),
        )))
    }

    async fn folding_range(&self, params: FoldingRangeParams) -> Result<Option<Vec<FoldingRange>>> {
        let document = self
            .documents
            .read()
            .await
            .get(&params.text_document.uri)
            .cloned();

        let Some(document) = document else {
            return Ok(None);
        };

        Ok(Some(folding::folding_ranges(
            &document.text,
            &document.analysis,
        )))
    }

    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        let document = self
            .documents
            .read()
            .await
            .get(&params.text_document.uri)
            .cloned();

        let Some(document) = document else {
            return Ok(None);
        };

        Ok(Some(folding::selection_ranges(
            &document.text,
            &document.analysis,
            &params.positions,
        )))
    }

    async fn semantic_tokens_range(
        &self,
        params: SemanticTokensRangeParams,
    ) -> Result<Option<SemanticTokensRangeResult>> {
        let document = self
            .documents
            .read()
            .await
            .get(&params.text_document.uri)
            .cloned();

        let Some(document) = document else {
            return Ok(None);
        };

        Ok(Some(
            SemanticTokens {
                result_id: None,
                data: semantic::tokens_in_range(
                    &document.text,
                    &document.analysis,
                    params.range.start.line,
                    params.range.end.line,
                ),
            }
            .into(),
        ))
    }

    async fn goto_declaration(
        &self,
        _params: GotoDeclarationParams,
    ) -> Result<Option<GotoDeclarationResponse>> {
        Ok(None)
    }

    async fn goto_implementation(
        &self,
        _params: GotoImplementationParams,
    ) -> Result<Option<GotoImplementationResponse>> {
        Ok(None)
    }

    async fn document_link(&self, params: DocumentLinkParams) -> Result<Option<Vec<DocumentLink>>> {
        let uri = params.text_document.uri;
        let documents = self.documents.read().await;

        let Some(document) = documents.get(&uri) else {
            return Ok(None);
        };

        let open_path = diagnostics::file_uri_to_path(&uri);
        let open_dir = open_path
            .as_ref()
            .and_then(|path| path.parent().map(|parent| parent.to_path_buf()));
        let project_root = open_path
            .as_ref()
            .map(|path| diagnostics::project_root_for(path, &document.text));
        let std_root = diagnostics::resolve_std_root();
        let zora_root = open_path.as_ref().and_then(|path| zora::find_root(path));
        let (packages, _) = self.packages_for(zora_root.as_deref()).await;

        let dir = match open_dir.as_deref() {
            Some(dir) => dir,
            None => return Ok(None),
        };

        let links = crate::actions::document_links(
            &document.text,
            Some(dir),
            project_root.as_deref(),
            std_root.as_deref(),
            &packages,
        );

        if links.is_empty() {
            return Ok(None);
        }

        let mut out = Vec::new();

        for ((start, end), _path, target) in links {
            let Ok(target_uri) = Uri::from_str(&format!("file://{}", target.display())) else {
                continue;
            };

            out.push(DocumentLink {
                range: position::span_to_range(&document.text, start, end - start),
                target: Some(target_uri),
                tooltip: None,
                data: None,
            });
        }

        if out.is_empty() {
            return Ok(None);
        }

        Ok(Some(out))
    }

    async fn document_link_resolve(&self, params: DocumentLink) -> Result<DocumentLink> {
        Ok(params)
    }

    async fn completion_resolve(&self, params: CompletionItem) -> Result<CompletionItem> {
        Ok(params)
    }

    async fn code_lens_resolve(&self, params: CodeLens) -> Result<CodeLens> {
        Ok(params)
    }

    async fn symbol_resolve(&self, params: WorkspaceSymbol) -> Result<WorkspaceSymbol> {
        Ok(params)
    }

    async fn inlay_hint_resolve(&self, params: InlayHint) -> Result<InlayHint> {
        Ok(params)
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let documents = self.documents.read().await;

        let Some(document) = documents.get(&uri) else {
            return Ok(None);
        };

        let offset = position::position_to_offset(
            &document.text,
            params.text_document_position_params.position,
        );
        let docs = hierarchy_docs(&documents);

        Ok(crate::hierarchy::prepare(&docs, &uri, offset).map(|item| vec![item]))
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        let documents = self.documents.read().await;
        let docs = hierarchy_docs(&documents);
        let calls = crate::hierarchy::incoming(&docs, &params.item);

        if calls.is_empty() {
            return Ok(None);
        }

        Ok(Some(calls))
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let documents = self.documents.read().await;
        let docs = hierarchy_docs(&documents);
        let calls = crate::hierarchy::outgoing(&docs, &params.item);

        if calls.is_empty() {
            return Ok(None);
        }

        Ok(Some(calls))
    }

    async fn prepare_type_hierarchy(
        &self,
        params: TypeHierarchyPrepareParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let uri = params.text_document_position_params.text_document.uri;
        let documents = self.documents.read().await;

        let Some(document) = documents.get(&uri) else {
            return Ok(None);
        };

        let offset = position::position_to_offset(
            &document.text,
            params.text_document_position_params.position,
        );
        let docs = hierarchy_docs(&documents);

        Ok(crate::hierarchy::prepare_type(&docs, &uri, offset).map(|item| vec![item]))
    }

    async fn supertypes(
        &self,
        params: TypeHierarchySupertypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let documents = self.documents.read().await;
        let docs = hierarchy_docs(&documents);
        let types = crate::hierarchy::supertypes(&docs, &params.item);

        if types.is_empty() {
            return Ok(None);
        }

        Ok(Some(types))
    }

    async fn subtypes(
        &self,
        params: TypeHierarchySubtypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let documents = self.documents.read().await;
        let docs = hierarchy_docs(&documents);
        let types = crate::hierarchy::subtypes(&docs, &params.item);

        if types.is_empty() {
            return Ok(None);
        }

        Ok(Some(types))
    }

    async fn moniker(&self, _params: MonikerParams) -> Result<Option<Vec<Moniker>>> {
        Ok(None)
    }

    async fn inline_value(&self, _params: InlineValueParams) -> Result<Option<Vec<InlineValue>>> {
        Ok(None)
    }

    async fn linked_editing_range(
        &self,
        _params: LinkedEditingRangeParams,
    ) -> Result<Option<LinkedEditingRanges>> {
        Ok(None)
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let document = self.documents.read().await.get(&uri).cloned();

        let Some(document) = document else {
            return Ok(None);
        };

        let offset = position::position_to_offset(
            &document.text,
            params.text_document_position_params.position,
        );

        let open_path = diagnostics::file_uri_to_path(&uri);
        let open_dir = open_path
            .as_ref()
            .and_then(|path| path.parent().map(|parent| parent.to_path_buf()));
        let project_root = open_path
            .as_ref()
            .map(|path| diagnostics::project_root_for(path, &document.text));
        let std_root = diagnostics::resolve_std_root();
        let zora_root = open_path.as_ref().and_then(|path| zora::find_root(path));
        let (packages, _) = self.packages_for(zora_root.as_deref()).await;
        let paths = crate::completion::ResolvePaths {
            dir: open_dir.as_deref(),
            root: project_root.as_deref(),
            std: std_root.as_deref(),
            packages: Some(packages.as_slice()),
        };

        let Some(result) = crate::hover::hover_markdown(
            &document.text,
            &document.analysis,
            offset,
            paths,
            &diagnostics::uri_filename(&uri),
        ) else {
            return Ok(None);
        };

        Ok(Some(Hover {
            contents: HoverContents::Array(vec![MarkedString::from_markdown(result.markdown)]),
            range: Some(position::span_to_range(
                &document.text,
                result.offset,
                result.len,
            )),
        }))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = params.text_document.uri.clone();
        let documents = self.documents.read().await;

        let Some(document) = documents.get(&uri) else {
            return Ok(None);
        };

        let start = position::position_to_offset(&document.text, params.range.start);
        let end = position::position_to_offset(&document.text, params.range.end);

        let mut hints = Vec::new();

        for call in &document.analysis.calls {
            if call.callee_offset < start || call.callee_offset > end {
                continue;
            }

            if let Some(generics) = document
                .analysis
                .call_generics
                .get(&(call.callee_offset, call.callee_len))
            {
                hints.push(InlayHint {
                    position: position::offset_to_position(
                        &document.text,
                        call.callee_offset + call.callee_len,
                    ),
                    label: InlayHintLabel::String(generics.clone()),
                    kind: Some(InlayHintKind::TYPE),
                    text_edits: None,
                    tooltip: None,
                    padding_left: None,
                    padding_right: None,
                    data: None,
                });
            }

            for (index, &(arg, _)) in call.args.iter().enumerate() {
                let name = call.params.get(index).and_then(|param| param.as_deref());
                let ty = call
                    .param_types
                    .get(index)
                    .and_then(|param| param.as_deref());

                let label = match (name, ty) {
                    (Some(name), _) => format!("{name}:"),
                    (None, Some(ty)) => format!(": {ty}"),
                    (None, None) => continue,
                };

                hints.push(InlayHint {
                    position: position::offset_to_position(&document.text, arg),
                    label: InlayHintLabel::String(label),
                    kind: Some(InlayHintKind::PARAMETER),
                    text_edits: None,
                    tooltip: None,
                    padding_left: None,
                    padding_right: Some(true),
                    data: None,
                });
            }
        }

        for occurrence in &document.analysis.occurrences {
            if !occurrence.type_hint {
                continue;
            }

            if occurrence.offset < start || occurrence.offset > end {
                continue;
            }

            let Some(ty) = &occurrence.ty else {
                continue;
            };

            hints.push(InlayHint {
                position: position::offset_to_position(
                    &document.text,
                    occurrence.offset + occurrence.len,
                ),
                label: InlayHintLabel::String(format!(": {ty}")),
                kind: Some(InlayHintKind::TYPE),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: None,
                data: None,
            });
        }

        for hint in &document.analysis.field_hints {
            if hint.offset < start || hint.offset > end {
                continue;
            }

            hints.push(InlayHint {
                position: position::offset_to_position(&document.text, hint.offset + hint.len),
                label: InlayHintLabel::String(format!(": {}", hint.ty)),
                kind: Some(InlayHintKind::TYPE),
                text_edits: None,
                tooltip: None,
                padding_left: None,
                padding_right: None,
                data: None,
            });
        }

        Ok(Some(hints))
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let position = params.text_document_position.position;
        let uri = params.text_document_position.text_document.uri.clone();
        let document = self.documents.read().await.get(&uri).cloned();

        let Some(document) = document else {
            return Ok(None);
        };

        let offset = position::position_to_offset(&document.text, position);
        let open_path = diagnostics::file_uri_to_path(&uri);
        let open_dir = open_path
            .as_ref()
            .and_then(|path| path.parent().map(|parent| parent.to_path_buf()));
        let project_root = open_path
            .as_ref()
            .map(|path| diagnostics::project_root_for(path, &document.text));
        let std_root = diagnostics::resolve_std_root();
        let zora_root = open_path.as_ref().and_then(|path| zora::find_root(path));
        let (packages, _) = self.packages_for(zora_root.as_deref()).await;
        let paths = completion::ResolvePaths {
            dir: open_dir.as_deref(),
            root: project_root.as_deref(),
            std: std_root.as_deref(),
            packages: Some(packages.as_slice()),
        };
        let items = if let Some(prefix) = completion::use_path_prefix(&document.text, offset) {
            let mut modules = completion::core_modules();

            if let Some(root) = diagnostics::resolve_std_root() {
                modules.extend(completion::std_modules(&root));
            }

            if let Some(path) = diagnostics::file_uri_to_path(&uri)
                && let (Some(dir), Some(stem)) = (
                    path.parent(),
                    path.file_stem().and_then(|stem| stem.to_str()),
                )
            {
                modules.extend(completion::sibling_modules(dir, stem));
            }

            if (prefix == "super" || prefix.starts_with("super."))
                && let Some(base) = open_dir.as_deref().and_then(|dir| dir.parent())
            {
                modules.extend(crate::modules::prefixed_modules(base, "super"));
            }

            if (prefix == "root" || prefix.starts_with("root."))
                && let Some(base) = project_root.as_deref()
            {
                modules.extend(crate::modules::prefixed_modules(base, "root"));
            }

            for (name, dir) in &packages {
                modules.push(name.clone());

                if prefix == *name || prefix.starts_with(&format!("{name}.")) {
                    modules.extend(crate::modules::prefixed_modules(dir, name));
                }
            }

            completion::complete_use(&prefix, &modules)
        } else if completion::struct_init_field_position(&document.text, offset) {
            let items =
                completion::struct_init_complete(&document.text, &document.analysis, offset, paths);

            if items.is_empty() {
                self.retry_with_repaired(
                    &uri,
                    &document.text,
                    offset,
                    paths,
                    CompletionBranch::StructInit,
                )
                .await
            } else {
                items
            }
        } else if let Some(trigger) = completion::call_dot_before(&document.text, offset) {
            let items =
                completion::call_complete(&document.text, &document.analysis, trigger, paths);

            if items.is_empty() {
                self.retry_with_repaired(
                    &uri,
                    &document.text,
                    offset,
                    paths,
                    CompletionBranch::Call(trigger),
                )
                .await
            } else {
                items
            }
        } else if let Some(dot) = completion::receiver_dot_before(&document.text, offset) {
            let items = completion::dot_complete(&document.text, &document.analysis, dot, paths);

            if items.is_empty() {
                self.retry_with_repaired(
                    &uri,
                    &document.text,
                    offset,
                    paths,
                    CompletionBranch::Dot(dot),
                )
                .await
            } else {
                items
            }
        } else {
            completion::complete(&document.text, &document.analysis, offset, paths)
        };

        Ok(Some(CompletionResponse::Array(items)))
    }

    async fn signature_help(&self, params: SignatureHelpParams) -> Result<Option<SignatureHelp>> {
        let position = params.text_document_position_params.position;
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let documents = self.documents.read().await;

        let Some(document) = documents.get(&uri) else {
            return Ok(None);
        };

        let offset = position::position_to_offset(&document.text, position);

        let Some(data) = document.analysis.signature_at(offset) else {
            return Ok(None);
        };

        let parameters = data
            .params
            .iter()
            .map(|param| ParameterInformation {
                label: ParameterLabel::Simple(param.clone()),
                documentation: None,
            })
            .collect();

        Ok(Some(SignatureHelp {
            signatures: vec![SignatureInformation {
                label: data.label,
                documentation: None,
                parameters: Some(parameters),
                active_parameter: None,
            }],
            active_signature: Some(0),
            active_parameter: data.active,
        }))
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        let text = params.text_document.text;

        let root = self.zora_root_of(&uri);
        let (packages, stderr) = self.packages_for(root.as_deref()).await;

        if let Some(message) = stderr {
            self.client.show_message(MessageType::ERROR, message).await;
        }

        let output = diagnostics::check(&uri, &text, &packages);

        self.documents.write().await.insert(
            uri.clone(),
            Document {
                version,
                text,
                analysis: output.analysis,
            },
        );

        self.client
            .publish_diagnostics(uri, output.diagnostics, None)
            .await;

        let _ = self.client.semantic_tokens_refresh().await;
        let _ = self.client.inlay_hint_refresh().await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let Some(change) = params.content_changes.into_iter().last() else {
            return;
        };

        let uri = params.text_document.uri;
        let version = params.text_document.version;

        {
            let mut documents = self.documents.write().await;
            let previous = documents
                .get(&uri)
                .map(|document| document.analysis.clone());

            documents.insert(
                uri.clone(),
                Document {
                    version,
                    text: change.text,
                    analysis: previous.unwrap_or_default(),
                },
            );
        }

        let client = self.client.clone();
        let documents = Arc::clone(&self.documents);
        let deps = Arc::clone(&self.deps);
        let version = params.text_document.version;
        let root = self.zora_root_of(&uri);

        tokio::spawn(async move {
            tokio::time::sleep(DIAGNOSTIC_DEBOUNCE).await;

            let current = documents.read().await.get(&uri).cloned();

            let Some(current) = current else {
                return;
            };

            if current.version != version {
                return;
            }

            let packages = packages_for_cache(&deps, root.as_deref()).await;
            let output = diagnostics::check(&uri, &current.text, &packages.0);

            let analysis = if output.analysis.occurrences.is_empty() {
                current.analysis.clone()
            } else {
                output.analysis
            };

            documents.write().await.insert(
                uri.clone(),
                Document {
                    version,
                    text: current.text,
                    analysis,
                },
            );

            client
                .publish_diagnostics(uri, output.diagnostics, None)
                .await;

            let _ = client.semantic_tokens_refresh().await;
            let _ = client.inlay_hint_refresh().await;
        });
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri;
        let documents = self.documents.read().await;

        let Some(document) = documents.get(&uri) else {
            return Ok(None);
        };

        let Some(open_path) = diagnostics::file_uri_to_path(&uri) else {
            return Ok(None);
        };

        let Some(open_dir) = open_path.parent() else {
            return Ok(None);
        };

        let project_root = diagnostics::project_root_for(&open_path, &document.text);
        let zora_root = zora::find_root(&open_path);
        let (packages, _) = self.packages_for(zora_root.as_deref()).await;

        let mut roots = vec![project_root.clone()];

        for (_, dir) in &packages {
            roots.push(dir.clone());
        }

        let mut index = self.symbols.write().await;
        index.refresh(&roots);

        let mut actions = Vec::new();

        for diagnostic in &params.context.diagnostics {
            if diagnostic.code
                != Some(NumberOrString::String(
                    "zeen::resolver::unresolved_ident".to_string(),
                ))
            {
                continue;
            }

            let start = position::position_to_offset(&document.text, diagnostic.range.start);
            let end = position::position_to_offset(&document.text, diagnostic.range.end);
            let Some(name) = document
                .text
                .get(start..end)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };

            actions.extend(crate::actions::missing_use_actions(
                &document.text,
                &uri,
                &index,
                open_dir,
                Some(&project_root),
                &packages,
                name,
            ));
        }

        if actions.is_empty() {
            return Ok(None);
        }

        Ok(Some(
            actions
                .into_iter()
                .map(CodeActionOrCommand::CodeAction)
                .collect(),
        ))
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .write()
            .await
            .remove(&params.text_document.uri);

        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await
    }

    async fn code_lens(&self, params: CodeLensParams) -> Result<Option<Vec<CodeLens>>> {
        let uri = params.text_document.uri;
        let documents = self.documents.read().await;

        let Some(document) = documents.get(&uri) else {
            return Ok(None);
        };

        let docs: Vec<crate::rename::RenameDoc<'_>> = documents
            .iter()
            .map(|(uri, document)| crate::rename::RenameDoc {
                uri: uri.clone(),
                text: &document.text,
                analysis: &document.analysis,
            })
            .collect();

        let mut lenses =
            crate::codelens::reference_lenses(&document.text, &document.analysis, &docs, &uri);

        if let Some(open_path) = diagnostics::file_uri_to_path(&uri) {
            let context = diagnostics::entry_context(&open_path, &document.text);
            let open_is_entry = context.entry == open_path;
            let runnable = match &context.zora {
                Some(root) => crate::codelens::runnable(Some(root), &document.text),
                None => crate::codelens::runnable(None, &document.text),
            };

            if let Some(lens) = crate::codelens::run_lens(
                &document.text,
                &document.analysis,
                &uri,
                open_is_entry,
                runnable,
            ) {
                lenses.push(lens);
            }
        }

        if lenses.is_empty() {
            return Ok(None);
        }

        Ok(Some(lenses))
    }

    async fn execute_command(&self, params: ExecuteCommandParams) -> Result<Option<LSPAny>> {
        match params.command.as_str() {
            crate::codelens::RUN_COMMAND => {
                self.run_main(params.arguments).await;
                Ok(None)
            }
            crate::codelens::REFERENCES_COMMAND => {
                self.show_references(params.arguments).await;
                Ok(None)
            }
            _ => Err(tower_lsp_server::jsonrpc::Error::method_not_found()),
        }
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        let mut roots = HashSet::new();
        let mut changed_sources: Vec<PathBuf> = Vec::new();

        for change in &params.changes {
            let Some(path) = diagnostics::file_uri_to_path(&change.uri) else {
                continue;
            };

            let is_manifest = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == zora::MANIFEST || name == zora::LOCKFILE);

            if is_manifest {
                if let Some(parent) = path.parent() {
                    roots.insert(parent.to_path_buf());
                }

                continue;
            }

            if path.extension().and_then(|ext| ext.to_str()) == Some("zn") {
                let canonical = std::fs::canonicalize(&path).unwrap_or(path);
                changed_sources.push(canonical);
            }
        }

        for root in roots {
            self.deps.write().await.remove(&root);
            let (packages, stderr) = self.packages_for(Some(&root)).await;

            if let Some(message) = stderr {
                self.client.show_message(MessageType::ERROR, message).await;
            } else {
                self.client
                    .show_message(
                        MessageType::INFO,
                        format!("dependencies updated: {} packages", packages.len()),
                    )
                    .await;
            }

            let open: Vec<(Uri, String, i32)> = {
                let documents = self.documents.read().await;

                documents
                    .iter()
                    .filter(|(uri, _)| {
                        diagnostics::file_uri_to_path(uri)
                            .is_some_and(|path| path.starts_with(&root))
                    })
                    .map(|(uri, document)| (uri.clone(), document.text.clone(), document.version))
                    .collect()
            };

            for (uri, text, version) in open {
                let output = diagnostics::check(&uri, &text, &packages);

                self.documents.write().await.insert(
                    uri.clone(),
                    Document {
                        version,
                        text,
                        analysis: output.analysis,
                    },
                );

                self.client
                    .publish_diagnostics(uri, output.diagnostics, None)
                    .await;
            }
        }

        if changed_sources.is_empty() {
            return;
        }

        let dependents = self.dependent_documents(&changed_sources).await;

        for (uri, text, version, packages) in dependents {
            let output = diagnostics::check(&uri, &text, &packages);

            self.documents.write().await.insert(
                uri.clone(),
                Document {
                    version,
                    text,
                    analysis: output.analysis,
                },
            );

            self.client
                .publish_diagnostics(uri, output.diagnostics, None)
                .await;
        }
    }
}
