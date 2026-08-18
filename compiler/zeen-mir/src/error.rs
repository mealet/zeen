use smol_str::SmolStr;
use std::sync::Arc;

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

#[derive(Debug, Error, Diagnostic, Clone)]
pub enum MirWarning {
    #[error("unused result of {what}")]
    #[diagnostic(
        severity(Warning),
        code(zeen::mir::unused_expression_result),
        help("consider using the value, or discarding it explicitly: `let _ = ...;`")
    )]
    UnusedExpressionResult {
        what: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },
}
