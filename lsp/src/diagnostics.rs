use crate::position;
use std::{cell::RefCell, rc::Rc, sync::Arc};

use miette::Diagnostic as _;
use tower_lsp_server::ls_types::{Diagnostic, DiagnosticSeverity};

pub fn check(text: impl AsRef<str>) -> Vec<Diagnostic> {
    let filename = Rc::new("in-memory.zn".to_string());
    let content = Arc::new(text.as_ref().to_string());
    let interner = Rc::new(RefCell::new(todo!()));

    todo!()
}
