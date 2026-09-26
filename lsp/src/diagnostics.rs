use std::{
    cell::RefCell,
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use crate::{analysis::Analysis, position};

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

pub fn check(uri: &Uri, text: &str) -> CheckOutput {
    let Some(entry_path) = file_uri_to_path(uri) else {
        return CheckOutput {
            diagnostics: check_syntax(uri, text),
            analysis: Analysis::default(),
        };
    };

    check_full(&entry_path, uri, text)
}

fn check_syntax(uri: &Uri, text: &str) -> Vec<Diagnostic> {
    let filename = Rc::new(uri_filename(uri));
    let content = Arc::new(text.to_string());
    let interner = Rc::new(RefCell::new(Rodeo::default()));
    let bump = Bump::new();

    let mut tokens = zeen_lexer::tokenize(text);
    let mut parser = zeen_parser::Parser::new(filename, content, &mut tokens, &bump, interner);

    match parser.parse_program() {
        Ok(_) => Vec::new(),
        Err(errors) => {
            let mut diags = Vec::with_capacity(errors.len());

            for err in errors.iter() {
                push_diagnostic(&mut diags, uri, text, err);
            }

            diags
        }
    }
}

fn check_full(entry_path: &Path, uri: &Uri, text: &str) -> CheckOutput {
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
                push_diagnostic(&mut diags, uri, text, err);
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

    let project_root = entry_path
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));

    let mut context = zeen_driver::CompilationContext {
        paths: zeen_driver::PathsConfig {
            project_root,
            std_root: resolve_std_root(),
            linked: HashSet::new(),
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
                push_diagnostic(&mut diags, uri, text, err);
            }
            return CheckOutput {
                diagnostics: diags,
                analysis: Analysis::default(),
            };
        }
    };

    let mut hir_lowering = zeen_hir::HirLowering::new(&resolution_result, Rc::clone(&interner));
    let hir_module = hir_lowering.lower_module(resolved_program);

    drop(bump);

    let mut typechecker =
        zeen_typecheck::TypeChecker::new(&mut resolution_result, &context, Rc::clone(&interner));

    typechecker.check_module(&hir_module);

    let mut typechecker_result = match typechecker.finish() {
        Ok(result) => result,
        Err(errors) => {
            for err in errors.iter() {
                push_diagnostic(&mut diags, uri, text, err);
            }
            return CheckOutput {
                diagnostics: diags,
                analysis: Analysis::default(),
            };
        }
    };

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
                push_diagnostic(&mut diags, uri, text, err);
            }
            return CheckOutput {
                diagnostics: diags,
                analysis: Analysis::default(),
            };
        }
    };

    for warning in lowered.warnings.iter() {
        push_diagnostic(&mut diags, uri, text, warning);
    }

    match zeen_flow::run_dataflow(
        &mut lowered.program,
        &mut typechecker_result,
        &resolution_result,
        Rc::clone(&interner),
    ) {
        Ok(flow_result) => {
            for warning in flow_result.warnings.iter() {
                push_diagnostic(&mut diags, uri, text, warning);
            }

            CheckOutput {
                diagnostics: diags,
                analysis: Analysis::default(),
            }
        }
        Err(errors) => {
            for err in errors.iter() {
                push_diagnostic(&mut diags, uri, text, err);
            }

            CheckOutput {
                diagnostics: diags,
                analysis: Analysis::default(),
            }
        }
    }
}

fn resolve_std_root() -> Option<PathBuf> {
    if let Some(env_path) = std::env::var_os("ZEEN_STD") {
        let path = PathBuf::from(env_path);
        if path.is_dir() {
            return Some(path);
        }
    }

    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let path = PathBuf::from(home).join(".zeen").join("std");
        if path.is_dir() {
            return Some(path);
        }
    }
    None
}

fn push_diagnostic(
    diags: &mut Vec<Diagnostic>,
    uri: &Uri,
    text: &str,
    err: &dyn miette::Diagnostic,
) {
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
                    related.push((related_message.clone(), label.offset(), label.len()));
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
                uri: uri.clone(),
                range: position::span_to_range(text, *offset, *len),
            },
            message: related_message.clone(),
        });
    }

    let severity = match err.severity().unwrap_or(Severity::Error) {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Advice => DiagnosticSeverity::HINT,
    };

    diags.push(Diagnostic {
        range: position::span_to_range(text, main.0, main.1),
        severity: Some(severity),
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

fn uri_filename(uri: &Uri) -> String {
    file_uri_to_path(uri)
        .and_then(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "in-memory.zn".to_string())
}

fn file_uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let stripped = uri.as_str().strip_prefix("file://")?;

    Some(PathBuf::from(decode_uri_path(stripped)))
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
