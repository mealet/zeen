use smol_str::SmolStr;
use std::{path::PathBuf, sync::Arc};

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

#[derive(Debug, Error, Diagnostic, Clone)]
pub enum ResolveError {
    // Transparent Errors
    #[error("parser error in included module")]
    #[diagnostic(transparent)]
    ModuleParseError(#[from] zeen_parser::error::ParserError),

    // --> Include Resolver Errors
    #[error("module is not found: `{path}`")]
    #[diagnostic(severity(Error), code(zeen::resolver::include_error))]
    FileNotFound {
        path: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("io error: `{message}`")]
    #[diagnostic(severity(Error), code(zeen::resolver::include_error))]
    IoError {
        message: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("duplicate of `{name}` declaration")]
    #[diagnostic(severity(Error), code(zeen::resolver::duplicate_definition))]
    DuplicateDefinition {
        name: SmolStr,

        #[related]
        related: Vec<DuplicateLocation>,
    },

    #[error("standard library is not configured")]
    #[diagnostic(severity(Error), code(zeen::resolver::std_not_configured))]
    StdlibNotConfigured {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },
}

#[derive(Debug, Error, Diagnostic, Clone)]
#[error("definition here")]
pub struct DuplicateLocation {
    #[source_code]
    pub src: NamedSource<String>,

    #[label]
    pub span: SourceSpan,
}
