use miette::{Diagnostic, NamedSource, SourceSpan};
use smol_str::SmolStr;
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic, Clone)]
pub enum Error {
    #[error("{message}")]
    #[diagnostic(severity(Error), code(zeen::comptime::panic))]
    Panic {
        message: SmolStr,

        #[related]
        trace: Vec<Trace>,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("{name}")]
    #[diagnostic(severity(Error), code(zeen::comptime::unresolved_ident))]
    UnresolvedIdent {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("{message}")]
    #[diagnostic(severity(Error), code(zeen::comptime::type_mismatch))]
    TypeMismatch {
        message: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("{message}")]
    #[diagnostic(severity(Error), code(zeen::comptime::limit_exceeded))]
    LimitExceeded {
        message: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("{message}")]
    #[diagnostic(severity(Error), code(zeen::comptime::runtime_only))]
    RuntimeOnly {
        message: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("{message}")]
    #[diagnostic(severity(Error), code(zeen::comptime::splice_outside_quote))]
    SpliceOutsideQuote {
        message: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("{message}")]
    #[diagnostic(severity(Error), code(zeen::comptime::format_parse_error))]
    FormatParseError {
        message: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("{message}")]
    #[diagnostic(severity(Error), code(zeen::comptime::unsupported))]
    Unsupported {
        message: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },
}

#[derive(Debug, Error, Diagnostic, Clone)]
#[error("called from here")]
#[diagnostic(severity(Advice))]
pub struct Trace {
    #[source_code]
    pub src: NamedSource<Arc<String>>,

    #[label("called from here")]
    pub span: SourceSpan,
}
