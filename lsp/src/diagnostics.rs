use crate::position;
use std::{cell::RefCell, rc::Rc, sync::Arc};

use bumpalo::Bump;
use lasso::Rodeo;
use miette::Diagnostic as _;
use tower_lsp_server::ls_types::{Diagnostic, DiagnosticSeverity};

pub fn check(text: impl AsRef<str>) -> Vec<Diagnostic> {
    let filename = Rc::new("in-memory.zn".to_string());
    let content = Arc::new(text.as_ref().to_string());
    let interner = Rc::new(RefCell::new(Rodeo::default()));
    let bump = Bump::new();

    let mut tokens = zeen_lexer::tokenize(text.as_ref());
    let mut parser = zeen_parser::Parser::new(filename, content, &mut tokens, &bump, interner);

    match parser.parse_program() {
        Ok(_) => Vec::new(),
        Err(errors) => errors
            .iter()
            .map(|err| {
                let message = err.to_string();
                let (offset, len) = err
                    .labels()
                    .and_then(|mut labels| labels.next())
                    .map(|label| (label.offset(), label.len()))
                    .unwrap_or((0, 0));

                Diagnostic {
                    range: position::span_to_range(text.as_ref(), offset, len),
                    severity: Some(DiagnosticSeverity::ERROR),
                    source: Some("zeen".to_string()),
                    message,
                    ..Default::default()
                }
            })
            .collect(),
    }
}
