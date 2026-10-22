use std::str::FromStr;

use rustc_hash::FxHashSet as HashSet;
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use crate::{
    analysis::{self, Analysis},
    position,
};

use bumpalo::Bump;
use lasso::Rodeo;
use miette::{Diagnostic as _, Severity};
use tower_lsp_server::ls_types::{
    Diagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location, NumberOrString, Uri,
};

pub struct CheckOutput {
    pub diagnostics: Vec<Diagnostic>,
    pub analysis: Analysis,
}

pub(crate) fn only_errors(diagnostics: &[Diagnostic]) -> Vec<&Diagnostic> {
    diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity != Some(DiagnosticSeverity::WARNING))
        .collect()
}

struct OpenFile<'a> {
    uri: &'a Uri,
    path: Option<PathBuf>,
    filename: &'a str,
    canonical: String,
    text: &'a str,
}

fn error_source_name(err: &dyn miette::Diagnostic, span: &miette::SourceSpan) -> Option<String> {
    let source = err.source_code()?;
    let contents = source.read_span(span, 0, 0).ok()?;
    contents.name().map(str::to_string)
}

fn severity_of(err: &dyn miette::Diagnostic) -> DiagnosticSeverity {
    match err.severity().unwrap_or(Severity::Error) {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Advice => DiagnosticSeverity::HINT,
    }
}

pub fn check(uri: &Uri, text: &str, packages: &[(String, PathBuf)]) -> CheckOutput {
    check_with(uri, text, true, packages)
}

pub fn check_completion(uri: &Uri, text: &str, packages: &[(String, PathBuf)]) -> CheckOutput {
    check_with(uri, text, false, packages)
}

pub(crate) fn parses(text: &str) -> bool {
    let filename = Rc::new(String::from("completion.zn"));
    let content = Arc::new(text.to_string());
    let interner = Rc::new(RefCell::new(Rodeo::default()));
    let bump = Bump::new();

    let mut tokens = zeen_lexer::tokenize(text);
    let mut parser =
        zeen_parser::Parser::new(Rc::clone(&filename), content, &mut tokens, &bump, interner);

    parser.parse_program().is_ok()
}

fn check_with(uri: &Uri, text: &str, full: bool, packages: &[(String, PathBuf)]) -> CheckOutput {
    let Some(open_path) = file_uri_to_path(uri) else {
        return CheckOutput {
            diagnostics: check_syntax(uri, text),
            analysis: Analysis::default(),
        };
    };

    let filename = uri_filename(uri);
    let canonical = std::fs::canonicalize(&open_path)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| filename.clone());
    let open = OpenFile {
        uri,
        path: Some(open_path.clone()),
        filename: &filename,
        canonical,
        text,
    };

    let context = entry_context(&open_path, text);

    check_entry(&context.entry, open, full, packages, &context.root)
}

fn check_entry(
    entry_path: &Path,
    open: OpenFile<'_>,
    full: bool,
    packages: &[(String, PathBuf)],
    project_root: &Path,
) -> CheckOutput {
    let open_path = open.path.clone().unwrap_or_else(|| PathBuf::from("."));

    if entry_path == open_path {
        return check_full(
            entry_path,
            open.uri,
            open.text,
            open,
            full,
            packages,
            project_root,
        );
    }

    let Ok(entry_text) = std::fs::read_to_string(entry_path) else {
        return check_full(
            &open_path,
            open.uri,
            open.text,
            open,
            full,
            packages,
            project_root,
        );
    };

    let entry_uri = Uri::from_str(&format!("file://{}", entry_path.display()))
        .unwrap_or_else(|_| open.uri.clone());

    check_full(
        entry_path,
        &entry_uri,
        &entry_text,
        open,
        full,
        packages,
        project_root,
    )
}

fn check_syntax(uri: &Uri, text: &str) -> Vec<Diagnostic> {
    let filename = Rc::new(uri_filename(uri));
    let content = Arc::new(text.to_string());
    let interner = Rc::new(RefCell::new(Rodeo::default()));
    let bump = Bump::new();

    let mut tokens = zeen_lexer::tokenize(text);
    let mut parser =
        zeen_parser::Parser::new(Rc::clone(&filename), content, &mut tokens, &bump, interner);

    let open = OpenFile {
        uri,
        path: file_uri_to_path(uri),
        filename: filename.as_str(),
        canonical: filename.to_string(),
        text,
    };

    match parser.parse_program() {
        Ok(_) => Vec::new(),
        Err(errors) => {
            let mut diags = Vec::with_capacity(errors.len());

            for err in errors.iter() {
                push_diagnostic(&mut diags, &open, err);
            }

            diags
        }
    }
}

fn check_full(
    entry_path: &Path,
    uri: &Uri,
    text: &str,
    open: OpenFile<'_>,
    full: bool,
    packages: &[(String, PathBuf)],
    project_root: &Path,
) -> CheckOutput {
    let filename = Rc::new(uri_filename(uri));
    let content = Arc::new(text.to_string());
    let interner = Rc::new(RefCell::new(Rodeo::default()));
    let bump = Bump::new();

    let mut diags = Vec::new();

    let mut tokens = zeen_lexer::tokenize(text);
    let mut parser = zeen_parser::Parser::new(
        Rc::clone(&filename),
        Arc::clone(&content),
        &mut tokens,
        &bump,
        Rc::clone(&interner),
    );

    let program = match parser.parse_program() {
        Ok(program) => program,
        Err(errors) => {
            for err in errors.iter() {
                push_diagnostic(&mut diags, &open, err);
            }

            return CheckOutput {
                diagnostics: diags,
                analysis: Analysis::default(),
            };
        }
    };

    let target = zeen_driver::Target::host();
    let program = zeen_preprocessor::resolve(
        program,
        &bump,
        &interner,
        &target,
        zeen_driver::CompilationMode::Debug,
    );

    let mut context = zeen_driver::CompilationContext {
        paths: zeen_driver::PathsConfig {
            project_root: project_root.to_path_buf(),
            std_root: resolve_std_root(),
            linked: HashSet::default(),
            packages: packages.to_vec(),
        },
        core_files: zeen_driver::CORE_FILES
            .iter()
            .map(|file| file.to_basic())
            .collect(),
        mode: zeen_driver::CompilationMode::Debug,
        output: zeen_driver::CompilationOutput::Binary,
        target: None,
        warnings: Vec::new(),
    };

    let (resolved_program, mut resolution_result) = match zeen_resolve::resolve(
        Rc::clone(&filename),
        Arc::clone(&content),
        entry_path,
        program,
        &bump,
        Rc::clone(&interner),
        &mut context,
    ) {
        Ok(resolved) => resolved,
        Err(errors) => {
            for err in errors.iter() {
                push_diagnostic(&mut diags, &open, err);
            }
            return CheckOutput {
                diagnostics: diags,
                analysis: analysis::build_syntax_fallback(program, text),
            };
        }
    };

    if open.path.as_deref() != Some(entry_path)
        && !resolution_result.defs.values().any(|info| {
            let name = info.span.src.name();
            name == open.filename || Some(name) == Some(open.canonical.as_str())
        })
    {
        let Some(open_path) = open.path.clone() else {
            return CheckOutput {
                diagnostics: diags,
                analysis: Analysis::default(),
            };
        };

        return check_full(
            &open_path,
            open.uri,
            open.text,
            open,
            full,
            packages,
            project_root,
        );
    }

    let mut hir_lowering = zeen_hir::HirLowering::new(&resolution_result, Rc::clone(&interner));
    let hir_module = hir_lowering.lower_module(resolved_program);

    drop(bump);

    let mut analysis = Analysis::build(
        &hir_module,
        &resolution_result,
        &interner,
        open.text,
        open.filename,
        &open.canonical,
    );

    let mut typechecker =
        zeen_typecheck::TypeChecker::new(&mut resolution_result, &context, Rc::clone(&interner));

    typechecker.check_module(&hir_module);

    let mut typechecker_result = match typechecker.finish() {
        Ok(result) => result,
        Err(errors) => {
            for err in errors.iter() {
                push_diagnostic(&mut diags, &open, err);
            }
            return CheckOutput {
                diagnostics: diags,
                analysis: analysis.clone(),
            };
        }
    };

    analysis.apply_types(
        &typechecker_result,
        Rc::clone(&interner),
        &resolution_result,
    );

    analysis.resolve_calls(
        &typechecker_result,
        Rc::clone(&interner),
        &resolution_result,
    );
    analysis.resolve_field_targets(
        &typechecker_result,
        Rc::clone(&interner),
        &resolution_result,
    );
    analysis.resolve_def_spans(&resolution_result, &filename, &open.canonical);
    analysis.resolve_fn_signatures(
        &typechecker_result,
        Rc::clone(&interner),
        &resolution_result,
    );
    analysis.resolve_enum_payloads(
        &typechecker_result,
        Rc::clone(&interner),
        &resolution_result,
    );
    analysis.resolve_member_types(
        &typechecker_result,
        Rc::clone(&interner),
        &resolution_result,
    );
    analysis.resolve_fields(
        &typechecker_result,
        Rc::clone(&interner),
        &resolution_result,
    );

    if !full {
        return CheckOutput {
            diagnostics: diags,
            analysis,
        };
    }

    let mut lowered = match zeen_mir::lowering::lower_program(
        Rc::clone(&interner),
        &mut typechecker_result,
        &resolution_result,
        &hir_module,
        context.mode,
    ) {
        Ok(lowered) => lowered,
        Err(errors) => {
            for err in errors.iter() {
                push_diagnostic(&mut diags, &open, err);
            }
            return CheckOutput {
                diagnostics: diags,
                analysis: analysis.clone(),
            };
        }
    };

    for warning in lowered
        .warnings
        .iter()
        .filter(|warning| !warning_module_disables(warning.source_code()))
    {
        push_diagnostic(&mut diags, &open, warning);
    }

    match zeen_flow::run_dataflow(
        &mut lowered.program,
        &mut typechecker_result,
        &resolution_result,
        Rc::clone(&interner),
    ) {
        Ok(flow_result) => {
            for warning in flow_result
                .warnings
                .iter()
                .filter(|warning| !warning_module_disables(warning.source_code()))
            {
                push_diagnostic(&mut diags, &open, warning);
            }

            CheckOutput {
                diagnostics: diags,
                analysis,
            }
        }
        Err(errors) => {
            for err in errors.iter() {
                push_diagnostic(&mut diags, &open, err);
            }

            CheckOutput {
                diagnostics: diags,
                analysis,
            }
        }
    }
}

fn warning_module_disables(source: Option<&miette::NamedSource<Arc<String>>>) -> bool {
    source.is_some_and(zeen_driver::source_disables_warnings)
}

pub(crate) fn std_src(root: PathBuf) -> Option<PathBuf> {
    let src = root.join("src");

    src.is_dir().then_some(src)
}

pub(crate) fn resolve_std_root() -> Option<PathBuf> {
    if let Some(env_path) = std::env::var_os("ZEEN_STD")
        && let Some(src) = std_src(PathBuf::from(env_path))
    {
        return Some(src);
    }

    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        && let Some(src) = std_src(PathBuf::from(home).join(".zeen").join("std"))
    {
        return Some(src);
    }

    None
}

fn push_diagnostic(diags: &mut Vec<Diagnostic>, open: &OpenFile<'_>, err: &dyn miette::Diagnostic) {
    let mut message = err.to_string();
    if let Some(help) = err.help() {
        message.push('\n');
        message.push_str(&help.to_string());
    }

    let mut primary: Vec<(usize, usize)> = Vec::new();
    if let Some(labels) = err.labels() {
        primary.extend(labels.map(|label| (label.offset(), label.len())));
    }
    let mut related: Vec<(String, usize, usize)> = Vec::new();
    if let Some(related_errors) = err.related() {
        for related_error in related_errors {
            let related_message = related_error.to_string();
            if let Some(labels) = related_error.labels() {
                for label in labels {
                    let span = miette::SourceSpan::new(label.offset().into(), label.len());
                    let matches = error_source_name(related_error, &span).is_none_or(|name| {
                        name == open.filename
                            || Some(name.as_str()) == Some(open.canonical.as_str())
                    });
                    if matches {
                        related.push((related_message.clone(), label.offset(), label.len()));
                    }
                }
            }
        }
    }

    let main = match primary.first() {
        Some(span) => *span,
        None => related
            .last()
            .map(|(_, offset, len)| (*offset, *len))
            .unwrap_or((0, 0)),
    };
    let probe = miette::SourceSpan::new(main.0.into(), main.1);
    if let Some(name) = error_source_name(err, &probe)
        && name != open.filename
        && Some(name.as_str()) != Some(open.canonical.as_str())
    {
        return;
    }
    let main_from_related = primary.is_empty() && !related.is_empty();

    let mut related_information = Vec::new();
    for (index, (related_message, offset, len)) in related.iter().enumerate() {
        if main_from_related && index == related.len() - 1 {
            continue;
        }
        if (*offset, *len) == main {
            continue;
        }
        related_information.push(DiagnosticRelatedInformation {
            location: Location {
                uri: open.uri.clone(),
                range: position::span_to_range(open.text, *offset, *len),
            },
            message: related_message.clone(),
        });
    }

    diags.push(Diagnostic {
        range: position::span_to_range(open.text, main.0, main.1),
        severity: Some(severity_of(err)),
        code: err
            .code()
            .map(|code| NumberOrString::String(code.to_string())),
        source: Some("zeen".to_string()),
        message,
        related_information: if related_information.is_empty() {
            None
        } else {
            Some(related_information)
        },
        ..Default::default()
    });
}

pub fn uri_filename(uri: &Uri) -> String {
    file_uri_to_path(uri)
        .and_then(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "in-memory.zn".to_string())
}

pub(crate) fn file_uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let stripped = uri.as_str().strip_prefix("file://")?;

    Some(PathBuf::from(decode_uri_path(stripped)))
}

pub(crate) struct EntryContext {
    pub entry: PathBuf,
    pub root: PathBuf,
    pub zora: Option<PathBuf>,
}

pub(crate) fn entry_context(open_path: &Path, text: &str) -> EntryContext {
    if let Some(root) = crate::zora::find_root(open_path) {
        let entry = crate::zora::entry_for_root(&root).unwrap_or_else(|| open_path.to_path_buf());

        return EntryContext {
            entry,
            root: root.clone(),
            zora: Some(root),
        };
    }

    let entry = discover_entry(open_path, text);
    let root = entry
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    EntryContext {
        entry,
        root,
        zora: None,
    }
}

pub(crate) fn project_root_for(open_path: &Path, open_text: &str) -> PathBuf {
    entry_context(open_path, open_text).root
}

fn discover_entry(open_path: &Path, open_text: &str) -> PathBuf {
    if contains_main(open_text) {
        return open_path.to_path_buf();
    }
    let Some(dir) = open_path.parent() else {
        return open_path.to_path_buf();
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("zn") {
                continue;
            }
            if path == open_path {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            if contains_main(&content) {
                candidates.push(path);
            }
        }
    }
    candidates.sort();
    if let Some(entry) = candidates.into_iter().next() {
        return entry;
    }
    let main_file = dir.join("main.zn");
    if main_file.is_file() {
        return main_file;
    }
    open_path.to_path_buf()
}

pub(crate) fn contains_main(text: &str) -> bool {
    text.contains("fn main(")
}

fn decode_uri_path(input: &str) -> String {
    let mut bytes = Vec::with_capacity(input.len());
    let mut index = 0;

    let raw = input.as_bytes();

    while index < raw.len() {
        if raw[index] == b'%'
            && index + 2 < raw.len()
            && let (Some(high), Some(low)) = (hex_value(raw[index + 1]), hex_value(raw[index + 2]))
        {
            bytes.push(high << 4 | low);
            index += 3;
            continue;
        }

        bytes.push(raw[index]);
        index += 1;
    }

    String::from_utf8_lossy(&bytes).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zeen-lsp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn contains_main_detects_entry() {
        assert!(contains_main("fn main() {}"));
        assert!(contains_main("fn main() i32 {\nreturn 0;\n}"));
        assert!(!contains_main("fn helper() {}"));
        assert!(!contains_main(""));
    }

    #[test]
    fn parses_accepts_valid_and_rejects_dangling_dot() {
        assert!(parses("fn main() {\n    let x = 1;\n}\n"));
        assert!(!parses("fn main() {\n    let x = foo.\n}\n"));
        assert!(!parses("fn main() {\n    let p = Point { .\n}\n"));
    }

    #[test]
    fn std_src_enters_project_src_directory() {
        let project = test_dir("std-src");
        std::fs::create_dir_all(project.join("src")).unwrap();

        assert_eq!(std_src(project.clone()), Some(project.join("src")));

        let bare = test_dir("std-src-bare");

        assert_eq!(std_src(bare), None);
    }

    #[test]
    fn check_completion_matches_full_analysis() {
        let text = "struct Point {\n    pub x: i32,\n}\n\nfn main() {\n    let p = Point { .x = 1 };\n    @println(\"{}\", p.x);\n}\n";
        let uri = Uri::from_str("file:///tmp/zeen-lsp-completion-parity.zn").unwrap();

        let full = check(&uri, text, &[]);
        assert!(full.diagnostics.is_empty());

        let light = check_completion(&uri, text, &[]);

        assert_eq!(
            light.analysis.occurrences.len(),
            full.analysis.occurrences.len()
        );
        assert_eq!(light.analysis.members.len(), full.analysis.members.len());
        assert!(!light.analysis.members.is_empty());
    }

    fn zora_lib_project(tag: &str, index: &str, testing: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("zeen-lsp-zoralib-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Zora.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nzeen = \"0.3\"\ntype = \"lib/static\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src").join("index.zn"), index).unwrap();
        let testing_path = dir.join("src").join("testing.zn");
        std::fs::write(&testing_path, testing).unwrap();

        (dir, testing_path)
    }

    #[test]
    fn lib_dependency_file_reports_errors() {
        let (_dir, testing_path) = zora_lib_project(
            "dep",
            "use testing;\n\npub fn helper() i32 {\n    return 1;\n}\n",
            "pub fn broken() i32 {\n    return undefined_variable;\n}\n",
        );
        let uri = Uri::from_str(&format!("file://{}", testing_path.display())).unwrap();
        let text = std::fs::read_to_string(&testing_path).unwrap();
        let output = check(&uri, &text, &[]);

        assert!(!output.diagnostics.is_empty());
        assert!(!output.analysis.occurrences.is_empty());
    }

    #[test]
    fn unreachable_file_checked_standalone() {
        let (_dir, testing_path) = zora_lib_project(
            "unreached",
            "pub fn helper() i32 {\n    return 1;\n}\n",
            "pub fn broken() i32 {\n    return undefined_variable;\n}\n",
        );
        let uri = Uri::from_str(&format!("file://{}", testing_path.display())).unwrap();
        let text = std::fs::read_to_string(&testing_path).unwrap();
        let output = check(&uri, &text, &[]);

        assert!(
            output
                .diagnostics
                .iter()
                .any(|diag| diag.message.contains("undefined_variable")),
            "standalone errors must show: {:?}",
            output.diagnostics
        );
        assert!(!output.analysis.occurrences.is_empty());
    }

    fn zora_bin_project(tag: &str, main: &str, extra: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("zeen-lsp-zorabin-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("Zora.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nzeen = \"0.3\"\ntype = \"bin\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("src").join("main.zn"), main).unwrap();
        let extra_path = dir.join("src").join("extra.zn");
        std::fs::write(&extra_path, extra).unwrap();

        (dir, extra_path)
    }

    #[test]
    fn bin_unreachable_file_checked_standalone() {
        let (_dir, extra_path) = zora_bin_project(
            "dep",
            "fn main() {\n    @println(\"hi\");\n}\n",
            "pub fn broken() i32 {\n    return undefined_variable;\n}\n",
        );
        let uri = Uri::from_str(&format!("file://{}", extra_path.display())).unwrap();
        let text = std::fs::read_to_string(&extra_path).unwrap();
        let output = check(&uri, &text, &[]);

        assert!(
            output
                .diagnostics
                .iter()
                .any(|diag| diag.message.contains("undefined_variable")),
            "standalone errors must show: {:?}",
            output.diagnostics
        );
    }

    #[test]
    fn bin_reachable_file_uses_entry_analysis() {
        let (_dir, extra_path) = zora_bin_project(
            "reached",
            "use extra;\n\nfn main() {\n    @println(\"{}\", extra.value());\n}\n",
            "pub fn value() i32 {\n    return 7;\n}\n",
        );
        let uri = Uri::from_str(&format!("file://{}", extra_path.display())).unwrap();
        let text = std::fs::read_to_string(&extra_path).unwrap();
        let output = check(&uri, &text, &[]);

        assert!(output.diagnostics.is_empty());
        assert!(!output.analysis.occurrences.is_empty());
    }

    #[test]
    fn discover_entry_prefers_open_file_with_main() {
        let dir = test_dir("prefer");
        let open = dir.join("lib.zn");
        std::fs::write(&open, "fn main() {}\n").unwrap();
        std::fs::write(dir.join("other.zn"), "fn main() {}\n").unwrap();
        assert_eq!(discover_entry(&open, "fn main() {}\n"), open);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_entry_finds_sibling_main() {
        let dir = test_dir("sibling");
        let open = dir.join("lib.zn");
        std::fs::write(&open, "fn helper() {}\n").unwrap();
        let main = dir.join("main.zn");
        std::fs::write(&main, "fn main() {}\n").unwrap();
        assert_eq!(discover_entry(&open, "fn helper() {}\n"), main);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_entry_falls_back_without_main() {
        let dir = test_dir("fallback");
        let open = dir.join("lib.zn");
        std::fs::write(&open, "fn helper() {}\n").unwrap();
        assert_eq!(discover_entry(&open, "fn helper() {}\n"), open);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discover_entry_falls_back_to_main_zn() {
        let dir = test_dir("mainname");
        let open = dir.join("lib.zn");
        std::fs::write(&open, "fn helper() {}\n").unwrap();
        let main = dir.join("main.zn");
        std::fs::write(&main, "fn helper() {}\n").unwrap();
        assert_eq!(discover_entry(&open, "fn helper() {}\n"), main);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_uri_to_path_decodes() {
        let uri = Uri::from_str("file:///tmp/zeen_test/test.zn").unwrap();
        assert_eq!(
            file_uri_to_path(&uri),
            Some(PathBuf::from("/tmp/zeen_test/test.zn"))
        );
    }

    #[test]
    fn file_uri_to_path_rejects_non_file() {
        let uri = Uri::from_str("untitled:Untitled-1").unwrap();
        assert_eq!(file_uri_to_path(&uri), None);
    }

    #[test]
    fn uri_filename_falls_back() {
        let uri = Uri::from_str("untitled:Untitled-1").unwrap();
        assert_eq!(uri_filename(&uri), "in-memory.zn");
        let uri = Uri::from_str("file:///tmp/a.zn").unwrap();
        assert_eq!(uri_filename(&uri), "a.zn");
    }

    #[test]
    fn error_source_name_reads_named_source() {
        use miette::NamedSource;
        use zeen_parser::error::ParserError;

        let err = ParserError::UnknownToken {
            src: NamedSource::new("sample.zn", Arc::new("?!?".to_string())),
            span: (0, 1).into(),
        };
        let probe = miette::SourceSpan::new(0.into(), 1);
        assert_eq!(
            error_source_name(&err, &probe),
            Some("sample.zn".to_string())
        );
    }
}
