use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::SystemTime;

use tower_lsp_server::ls_types::{
    DocumentSymbol, Location, Range, SymbolInformation, SymbolKind, Uri,
};

use crate::analysis::{Analysis, MemberKind, Role, TypeKind};
use crate::position;

#[allow(deprecated)]
pub fn document_symbols(text: &str, analysis: &Analysis) -> Vec<DocumentSymbol> {
    let mut by_def: HashMap<zeen_resolve::DefId, &crate::analysis::Occurrence> = HashMap::new();

    for occurrence in &analysis.occurrences {
        if let Some(def) = occurrence.def {
            by_def.entry(def).or_insert(occurrence);
        }
    }

    for occurrence in &analysis.occurrences {
        if let Some(def) = occurrence.def
            && occurrence.target_offset.is_none()
        {
            by_def.insert(def, occurrence);
        }
    }

    let mut children: HashMap<zeen_resolve::DefId, Vec<DocumentSymbol>> = HashMap::new();

    for (owner, members) in &analysis.members {
        let mut owned: Vec<DocumentSymbol> = Vec::new();

        for member in members {
            let Some(decl) = member_owner_decl(analysis, &by_def, member.def) else {
                continue;
            };

            let (kind, detail) = match member.kind {
                MemberKind::Method => (SymbolKind::METHOD, member.signature.clone()),
                MemberKind::Field => (SymbolKind::FIELD, member.signature.clone()),
                MemberKind::Variant => (SymbolKind::ENUM_MEMBER, None),
            };

            owned.push(DocumentSymbol {
                name: member.name.clone(),
                detail,
                kind,
                tags: None,
                deprecated: None,
                range: position::span_to_range(text, decl.offset, decl.len),
                selection_range: position::span_to_range(text, decl.offset, decl.len),
                children: None,
            });
        }

        owned.sort_by_key(|symbol| (symbol.range.start.line, symbol.range.start.character));

        if !owned.is_empty() {
            children.insert(*owner, owned);
        }
    }

    let mut top = Vec::new();

    for occurrence in &analysis.occurrences {
        let Some(def) = occurrence.def else {
            continue;
        };

        if occurrence.target_offset.is_some() {
            continue;
        }

        if analysis.member_owner.contains_key(&def) {
            continue;
        }

        let Some(name) = text
            .get(occurrence.offset..occurrence.offset + occurrence.len)
            .filter(|name| !name.is_empty())
        else {
            continue;
        };

        let (kind, detail) = match occurrence.role {
            Role::Function => (
                SymbolKind::FUNCTION,
                analysis
                    .fn_source_sig
                    .get(&def)
                    .cloned()
                    .or_else(|| occurrence.ty.clone()),
            ),
            Role::Method => (SymbolKind::METHOD, None),
            Role::Type => (
                match analysis.type_kinds.get(&def) {
                    Some(TypeKind::Enum) => SymbolKind::ENUM,
                    Some(TypeKind::Interface) => SymbolKind::INTERFACE,
                    Some(TypeKind::Alias) => SymbolKind::STRUCT,
                    _ => SymbolKind::STRUCT,
                },
                None,
            ),
            Role::Variable => match occurrence.is_const {
                Some(true) => (SymbolKind::CONSTANT, occurrence.ty.clone()),
                _ => (SymbolKind::VARIABLE, occurrence.ty.clone()),
            },
            Role::Parameter => continue,
            Role::Property => continue,
            Role::EnumMember => continue,
        };

        let range = scope_range(text, analysis, def, occurrence);

        top.push(DocumentSymbol {
            name: name.to_string(),
            detail,
            kind,
            tags: None,
            deprecated: None,
            range,
            selection_range: position::span_to_range(text, occurrence.offset, occurrence.len),
            children: children.remove(&def),
        });
    }

    top.sort_by_key(|symbol| (symbol.range.start.line, symbol.range.start.character));
    top
}

fn member_owner_decl<'a>(
    analysis: &'a Analysis,
    by_def: &HashMap<zeen_resolve::DefId, &'a crate::analysis::Occurrence>,
    def: zeen_resolve::DefId,
) -> Option<&'a crate::analysis::Occurrence> {
    by_def.get(&def).copied()
}

fn scope_range(
    text: &str,
    analysis: &Analysis,
    def: zeen_resolve::DefId,
    occurrence: &crate::analysis::Occurrence,
) -> Range {
    if let Some(scope) = analysis.scopes.iter().find(|scope| {
        scope.owner == def && scope.start <= occurrence.offset && occurrence.offset <= scope.end
    }) {
        return position::span_to_range(text, scope.start, scope.end - scope.start);
    }

    position::span_to_range(text, occurrence.offset, occurrence.len)
}

#[derive(Debug, Clone)]
pub struct IndexedSymbol {
    pub name: String,
    pub kind: SymbolKind,
    pub container: Option<String>,
    pub is_pub: bool,
    pub offset: usize,
    pub len: usize,
}

#[derive(Debug, Clone, Default)]
pub struct FileIndex {
    pub mtime: Option<SystemTime>,
    pub text: String,
    pub symbols: Vec<IndexedSymbol>,
}

#[derive(Debug, Clone)]
pub struct SymbolIndex {
    files: HashMap<PathBuf, FileIndex>,
    dirty: bool,
}

impl Default for SymbolIndex {
    fn default() -> Self {
        Self {
            files: HashMap::new(),
            dirty: true,
        }
    }
}

const MAX_INDEX_FILES: usize = 500;

impl SymbolIndex {
    pub fn files(&self) -> impl Iterator<Item = (&PathBuf, &FileIndex)> {
        self.files.iter()
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    pub fn ensure(&mut self, roots: &[PathBuf]) {
        if self.dirty {
            self.refresh(roots);
        }
    }

    pub fn refresh(&mut self, roots: &[PathBuf]) {
        let mut seen = HashSet::new();
        let mut count = 0;

        for root in roots {
            Self::collect(root, &mut seen, &mut count);

            if count >= MAX_INDEX_FILES {
                break;
            }
        }

        self.files.retain(|path, _| seen.contains(path));

        for path in seen {
            if count >= MAX_INDEX_FILES {
                break;
            }

            let fresh = file_mtime(&path);

            match self.files.get(&path) {
                Some(cached) if cached.mtime == fresh => {}
                _ => {
                    if let Some(indexed) = index_file(&path) {
                        self.files.insert(path.clone(), indexed);
                    } else {
                        self.files.remove(&path);
                    }

                    count += 1;
                }
            }
        }

        self.dirty = false;
    }

    fn collect(dir: &Path, seen: &mut HashSet<PathBuf>, count: &mut usize) {
        if *count >= MAX_INDEX_FILES {
            return;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            if *count >= MAX_INDEX_FILES {
                return;
            }

            let path = entry.path();

            let Some(name) = path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.starts_with('.'))
            else {
                continue;
            };

            if path.is_dir() {
                if name == "target" {
                    continue;
                }

                Self::collect(&path, seen, count);
                continue;
            }

            if path.extension().and_then(|ext| ext.to_str()) != Some("zn") {
                continue;
            }

            *count += 1;
            seen.insert(path);
        }
    }

    pub fn query(&self, query: &str) -> Vec<(Uri, IndexedSymbol, String)> {
        let needle = query.to_lowercase();
        let mut out = Vec::new();

        for (path, file) in &self.files {
            let Ok(uri) = Uri::from_str(&format!("file://{}", path.display())) else {
                continue;
            };

            for symbol in &file.symbols {
                if symbol.name.to_lowercase().contains(&needle) {
                    out.push((uri.clone(), symbol.clone(), file.text.clone()));
                }
            }
        }

        out.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        out
    }
}

fn file_mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

fn index_file(path: &Path) -> Option<FileIndex> {
    let content = std::fs::read_to_string(path).ok()?;
    let filename = std::rc::Rc::new(path.to_string_lossy().into_owned());
    let source = std::sync::Arc::new(content.clone());
    let interner = std::rc::Rc::new(std::cell::RefCell::new(lasso::Rodeo::default()));
    let bump = bumpalo::Bump::new();
    let mut tokens = zeen_lexer::tokenize(&content);

    let mut parser = zeen_parser::Parser::new(
        std::rc::Rc::clone(&filename),
        std::sync::Arc::clone(&source),
        &mut tokens,
        &bump,
        std::rc::Rc::clone(&interner),
    );

    let program = parser.parse_program().ok()?;
    let interner = interner.borrow();
    let mut symbols = Vec::new();

    for decl in program {
        push_decl_symbols(&content, &interner, decl, None, &mut symbols);
    }
    symbols.sort_by_key(|symbol: &IndexedSymbol| symbol.offset);

    Some(FileIndex {
        mtime: file_mtime(path),
        text: content.clone(),
        symbols,
    })
}

fn find_in_span(content: &str, span: miette::SourceSpan, name: &str) -> Option<(usize, usize)> {
    let start = span.offset().min(content.len());
    let end = start.saturating_add(span.len()).min(content.len());
    let haystack = content.get(start..end)?;
    let bytes = haystack.as_bytes();
    let mut search = 0;

    while let Some(position) = haystack[search..].find(name) {
        let absolute = search + position;
        let before_ok = absolute == 0
            || !(bytes[absolute - 1].is_ascii_alphanumeric() || bytes[absolute - 1] == b'_');
        let after = absolute + name.len();
        let after_ok = after >= haystack.len()
            || !(bytes[after].is_ascii_alphanumeric() || bytes[after] == b'_');

        if before_ok && after_ok {
            return Some((start + absolute, name.len()));
        }

        search = absolute + 1;
    }

    None
}

fn push_decl_symbols(
    content: &str,
    interner: &lasso::Rodeo,
    decl: &zeen_ast::Declaration<'_>,
    container: Option<String>,
    out: &mut Vec<IndexedSymbol>,
) {
    use zeen_ast::declarations::DeclarationKind;

    let resolve = |spur: &lasso::Spur| interner.resolve(spur).to_string();

    match &decl.kind {
        DeclarationKind::FnDecl { name, is_pub, .. } => {
            out.push(IndexedSymbol {
                name: resolve(&name.0),
                kind: SymbolKind::FUNCTION,
                container,
                is_pub: *is_pub,
                offset: name.1.offset(),
                len: name.1.len(),
            });
        }
        DeclarationKind::StructDecl {
            name,
            fields,
            methods,
            is_pub,
            ..
        } => {
            let struct_name = resolve(&name.0);

            out.push(IndexedSymbol {
                name: struct_name.clone(),
                kind: SymbolKind::STRUCT,
                container: container.clone(),
                is_pub: *is_pub,
                offset: name.1.offset(),
                len: name.1.len(),
            });

            for field in *fields {
                let field_name = resolve(&field.name);
                let (offset, len) = find_in_span(content, decl.source.span, &field_name)
                    .unwrap_or((name.1.offset(), name.1.len()));

                out.push(IndexedSymbol {
                    name: field_name,
                    kind: SymbolKind::FIELD,
                    container: Some(struct_name.clone()),
                    is_pub: field.is_pub,
                    offset,
                    len,
                });
            }

            for method in *methods {
                push_decl_symbols(content, interner, method, Some(struct_name.clone()), out);
            }
        }
        DeclarationKind::EnumDecl {
            name,
            variants,
            methods,
            is_pub,
            ..
        } => {
            let enum_name = resolve(&name.0);

            out.push(IndexedSymbol {
                name: enum_name.clone(),
                kind: SymbolKind::ENUM,
                container: container.clone(),
                is_pub: *is_pub,
                offset: name.1.offset(),
                len: name.1.len(),
            });

            for variant in *variants {
                out.push(IndexedSymbol {
                    name: resolve(&variant.name),
                    kind: SymbolKind::ENUM_MEMBER,
                    container: Some(enum_name.clone()),
                    is_pub: true,
                    offset: variant.span.offset(),
                    len: variant.span.len(),
                });
            }

            for method in *methods {
                push_decl_symbols(content, interner, method, Some(enum_name.clone()), out);
            }
        }
        DeclarationKind::InterfaceDecl {
            name,
            methods,
            is_pub,
            ..
        } => {
            let iface_name = resolve(&name.0);

            out.push(IndexedSymbol {
                name: iface_name.clone(),
                kind: SymbolKind::INTERFACE,
                container: container.clone(),
                is_pub: *is_pub,
                offset: name.1.offset(),
                len: name.1.len(),
            });

            for method in *methods {
                push_decl_symbols(content, interner, method, Some(iface_name.clone()), out);
            }
        }
        DeclarationKind::ImplementDecl { methods, .. } => {
            for method in *methods {
                push_decl_symbols(content, interner, method, container.clone(), out);
            }
        }
        DeclarationKind::Alias(alias) => {
            out.push(IndexedSymbol {
                name: resolve(&alias.name.0),
                kind: SymbolKind::STRUCT,
                container,
                is_pub: alias.is_pub,
                offset: alias.name.1.offset(),
                len: alias.name.1.len(),
            });
        }
        DeclarationKind::GlobalVar { name, is_pub, .. }
        | DeclarationKind::ExternVar { name, is_pub, .. } => {
            out.push(IndexedSymbol {
                name: resolve(&name.0),
                kind: SymbolKind::VARIABLE,
                container,
                is_pub: *is_pub,
                offset: name.1.offset(),
                len: name.1.len(),
            });
        }
        _ => {}
    }
}

#[allow(deprecated)]
pub fn workspace_symbols(index: &SymbolIndex, query: &str) -> Vec<SymbolInformation> {
    index
        .query(query)
        .into_iter()
        .map(|(uri, symbol, text)| SymbolInformation {
            name: symbol.name.clone(),
            kind: symbol.kind,
            tags: None,
            deprecated: None,
            location: Location {
                uri,
                range: position::span_to_range(&text, symbol.offset, symbol.len),
            },
            container_name: symbol.container.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "struct Point {\n    pub x: i32,\n    secret: i32,\n\n    pub fn make(x: i32, s: i32) Point {\n        return Point { .x = x, .secret = s };\n    }\n\n    pub fn get(*self) i32 {\n        return self.x;\n    }\n}\n\nenum Color {\n    red,\n    green,\n}\n\nfn add(a: i32, b: i32) i32 {\n    return a + b;\n}\n\nfn main() {\n    let p = Point.make(1, 2);\n    @println(\"{}\", p.x);\n}\n";

    fn sample_analysis(text: &str) -> crate::analysis::Analysis {
        let uri: Uri = "file:///tmp/zeen-lsp-symbols-probe.zn".parse().unwrap();
        let output = crate::diagnostics::check(&uri, text, &[]);
        assert!(output.diagnostics.is_empty());
        output.analysis
    }
    fn names(symbols: &[DocumentSymbol]) -> Vec<&str> {
        symbols.iter().map(|symbol| symbol.name.as_str()).collect()
    }

    #[test]
    fn document_nests_members() {
        let analysis = sample_analysis(SAMPLE);
        let symbols = document_symbols(SAMPLE, &analysis);
        let top = names(&symbols);

        assert!(top.contains(&"Point"));
        assert!(top.contains(&"Color"));
        assert!(top.contains(&"add"));
        assert!(top.contains(&"main"));
        assert!(!top.contains(&"x"));

        let point = symbols
            .iter()
            .find(|symbol| symbol.name == "Point")
            .unwrap();
        assert_eq!(point.kind, SymbolKind::STRUCT);

        let kids = names(point.children.as_deref().unwrap_or(&[]));
        assert!(kids.contains(&"x"));
        assert!(kids.contains(&"make"));
        assert!(kids.contains(&"get"));

        let color = symbols
            .iter()
            .find(|symbol| symbol.name == "Color")
            .unwrap();
        assert_eq!(color.kind, SymbolKind::ENUM);

        let add = symbols.iter().find(|symbol| symbol.name == "add").unwrap();
        assert_eq!(add.kind, SymbolKind::FUNCTION);
        assert!(add.detail.as_deref().unwrap_or("").contains("fn add"));
    }

    #[test]
    fn workspace_searches_files() {
        let dir = std::env::temp_dir().join(format!("zeen-lsp-symbols-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("main.zn"), "fn main() {}\n").unwrap();
        std::fs::write(
            dir.join("sub").join("lib.zn"),
            "pub struct Helper {\n    pub x: i32,\n}\n",
        )
        .unwrap();

        let mut index = SymbolIndex::default();
        index.refresh(std::slice::from_ref(&dir));

        let found = index.query("help");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1.name, "Helper");
        assert_eq!(found[0].1.container, None);

        let infos = workspace_symbols(&index, "mai");
        assert!(infos.iter().any(|info| info.name == "main"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ensure_reuses_index_until_dirty() {
        let dir =
            std::env::temp_dir().join(format!("zeen-lsp-symbols-lazy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.zn"), "fn alpha() {}\n").unwrap();

        let mut index = SymbolIndex::default();
        index.ensure(std::slice::from_ref(&dir));
        assert_eq!(index.query("alpha").len(), 1);

        std::fs::write(dir.join("main.zn"), "fn beta() {}\n").unwrap();
        index.ensure(std::slice::from_ref(&dir));
        assert!(index.query("beta").is_empty());

        index.mark_dirty();
        index.ensure(std::slice::from_ref(&dir));
        assert_eq!(index.query("beta").len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
