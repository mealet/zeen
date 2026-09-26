use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

use crate::position;

use bumpalo::Bump;
use lasso::Rodeo;
use miette::Diagnostic as _;
use tower_lsp_server::ls_types::{Diagnostic, DiagnosticSeverity, NumberOrString, Uri};

pub fn check(uri: &Uri, text: &str) -> Vec<Diagnostic> {
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
                push_diagnostic(&mut diags, text, err);
            }

            diags
        }
    }
}

fn push_diagnostic(diags: &mut Vec<Diagnostic>, text: &str, err: &dyn miette::Diagnostic) {
    let mut message = err.to_string();

    if let Some(help) = err.help() {
        message.push('\n');
        message.push_str(&help.to_string());
    }

    let (offset, len) = err
        .labels()
        .and_then(|mut labels| labels.next())
        .map(|label| (label.offset(), label.len()))
        .unwrap_or((0, 0));

    diags.push(Diagnostic {
        range: position::span_to_range(text, offset, len),
        severity: Some(DiagnosticSeverity::ERROR),
        message,
        source: Some("zeen".to_string()),
        code: err
            .code()
            .map(|code| NumberOrString::String(code.to_string())),
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
