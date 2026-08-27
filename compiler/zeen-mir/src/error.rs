use smol_str::SmolStr;
use std::sync::Arc;

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

#[derive(Debug, Error, Diagnostic, Clone)]
pub enum MirError {
    #[error("generic parameter is not a value")]
    #[diagnostic(severity(Error), code(zeen::mir::generic_param_not_a_value))]
    GenericParamNotValue {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },
}

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
