use smol_str::SmolStr;
use std::sync::Arc;

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

    // TODO: Help's link must be replaced when docs are out
    #[error("name `{name}` is reserved by compiler's core")]
    #[diagnostic(
        severity(Error),
        code(zeen::resolver::core_reserved),
        help("see compiler's core libraries at: https://github.com/mealet/zeen")
    )]
    CoreReserved {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("standard library is not configured")]
    #[diagnostic(severity(Error), code(zeen::resolver::std_not_configured))]
    StdlibNotConfigured {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("extern link error: {message}")]
    #[diagnostic(severity(Error), code(zeen::resolver::extern_link_error))]
    LinkError {
        message: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    // --> Name Resolver Error
    #[error("unresolved identifier: {name}")]
    #[diagnostic(severity(Error), code(zeen::resolver::unresolved_ident))]
    UnresolvedIdent {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("unresolved type: {name}")]
    #[diagnostic(severity(Error), code(zeen::resolver::unresolved_type))]
    UnresolvedType {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("unresolved `self/Self` type")]
    #[diagnostic(severity(Error), code(zeen::resolver::unresolved_self))]
    UnresolvedSelf {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("this feature is disabled by compiler: {reason}")]
    #[diagnostic(
        severity(Error),
        code(zeen::resolver::disabled_feature),
        help("visit for more information: https://github.com/mealet/zeen")
    )]
    DisabledFeature {
        reason: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label("\"{reason}\"")]
        span: SourceSpan,
    },

    #[error("usage of compiler-reserved interface name: `{name}`")]
    #[diagnostic(severity(Error), code(zeen::resolver::reserved_interface))]
    ReservedInterface {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("private item is not accessible here: `{name}`")]
    #[diagnostic(severity(Error), code(zeen::resolver::private_item))]
    PrivateItemNotAccessible {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("nested function cannot capture `{name}` from the enclosing function")]
    #[diagnostic(
        severity(Error),
        code(zeen::resolver::nested_fn_capture),
        help("nested functions cannot close over the enclosing function's variables")
    )]
    NestedFnCapture {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },
}

#[derive(Debug, Error, Diagnostic, Clone)]
#[error("definition here")]
#[diagnostic(severity(Advice))]
pub struct DuplicateLocation {
    #[source_code]
    pub src: NamedSource<String>,

    #[label]
    pub span: SourceSpan,
}
