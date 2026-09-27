use std::{collections::HashMap, sync::Arc, time::Duration};

use tokio::sync::RwLock;
use tower_lsp_server::{Client, LanguageServer, jsonrpc::Result, ls_types::*};

use crate::{analysis, diagnostics, position, semantic};

const DIAGNOSTIC_DEBOUNCE: Duration = Duration::from_millis(30);

#[derive(Debug)]
pub struct Backend {
    client: Client,
    documents: Arc<RwLock<HashMap<Uri, Document>>>,
}

#[derive(Debug, Clone)]
struct Document {
    version: i32,
    text: String,
    analysis: analysis::Analysis,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl LanguageServer for Backend {
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
                hover_provider: Some(HoverProviderCapability::Simple(true)),
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

        let (Some(target_offset), Some(target_len)) =
            (occurence.target_offset, occurence.target_len)
        else {
            return Ok(None);
        };

        Ok(Some(GotoDefinitionResponse::Array(vec![Location {
            uri,
            range: position::span_to_range(&document.text, target_offset, target_len),
        }])))
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
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

        let name = document
            .text
            .get(occurrence.offset..occurrence.offset + occurrence.len)
            .unwrap_or("");

        let mut contents = vec![MarkedString::from_language_code(
            "zeen".to_string(),
            name.to_string(),
        )];

        let mut detail = format!("{} - defined", occurrence.role.label());

        if let Some(target_offset) = occurrence.target_offset {
            let target = position::offset_to_position(&document.text, target_offset);
            let preview = position::line_text(&document.text, target.line).trim();
            let file = diagnostics::uri_filename(&uri);

            detail.push_str(&format!(
                " at {}:{}\n----\n```zn\n{}\n```",
                file,
                target.line + 1,
                preview
            ));
        }

        contents.push(MarkedString::from_markdown(detail));

        Ok(Some(Hover {
            contents: HoverContents::Array(contents),
            range: Some(position::span_to_range(
                &document.text,
                occurrence.offset,
                occurrence.len,
            )),
        }))
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        let text = params.text_document.text;

        let output = diagnostics::check(&uri, &text);

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
        let version = params.text_document.version;

        tokio::spawn(async move {
            tokio::time::sleep(DIAGNOSTIC_DEBOUNCE).await;

            let current = documents.read().await.get(&uri).cloned();

            let Some(current) = current else {
                return;
            };

            if current.version != version {
                return;
            }

            let output = diagnostics::check(&uri, &current.text);

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
        });
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
}
