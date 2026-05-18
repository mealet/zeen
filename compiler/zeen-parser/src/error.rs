use smol_str::SmolStr;
use std::sync::Arc;

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

#[derive(Debug, Error, Diagnostic, Clone)]
pub enum ParserError {
    #[error("unknown token found")]
    #[diagnostic(severity(Error), code(zeen::parser::unknown_token))]
    UnknownToken {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label("right here")]
        span: SourceSpan,
    },

    #[error("unknown expression found")]
    #[diagnostic(severity(Error), code(zeen::parser::unknown_expression))]
    UnknownExpression {
        token_kind: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label("[`{token_kind}`]")]
        span: SourceSpan,
    },

    #[error("expected `{expected}` token")]
    #[diagnostic(severity(Error), code(zeen::parser::expected_token))]
    ExpectedToken {
        expected: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label("but found this")]
        span: SourceSpan,
    },

    #[error("expected `{expected}`, but reached end of file")]
    #[diagnostic(severity(Error), code(zeen::parser::unexpected_eof))]
    UnexpectedEof {
        expected: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label("ended up here")]
        span: SourceSpan,
    },

    #[error("{message}")]
    #[diagnostic(severity(Error), code(zeen::parser::invalid_literal))]
    InvalidLiteral {
        message: SmolStr,
        label: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label("{label}")]
        span: SourceSpan,
    },

    #[error("invalid character escape")]
    #[diagnostic(severity(Error), code(zeen::parser::invalid_character_escape))]
    InvalidCharacterEscape {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label("this escape is invalid")]
        span: SourceSpan,
    },

    #[error("syntax error")]
    #[diagnostic(severity(Error), code(zeen::parser::syntax_error))]
    SyntaxError {
        label: SmolStr,

        #[help]
        help: Option<SmolStr>,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label("{label}")]
        span: SourceSpan,
    },

    #[error("unknown data type")]
    #[diagnostic(severity(Error), code(zeen::parser::unknown_type))]
    UnknownType {
        label: SmolStr,

        #[help]
        help: Option<SmolStr>,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label("{label}")]
        span: SourceSpan,
    },
}
