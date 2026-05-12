#![allow(unused)]

use zeen_driver::MietteDriver;

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
#[error("{dbg}")]
#[diagnostic(severity(Advice))]
struct SrcDebugger {
    dbg: String,

    #[source_code]
    src: NamedSource<String>,
    #[label("here")]
    span: SourceSpan,
}

fn main() {
    const SRC: &str = "implement";

    let tokens = zeen_lexer::tokenize(SRC);

    let driver = MietteDriver::new();

    for tok in tokens {
        let diagnostic = driver
            .report(&SrcDebugger {
                dbg: format!("{:?}", tok.kind),
                src: NamedSource::new("debug", SRC.into()),
                span: tok.span,
            })
            .unwrap();

        println!("{}", diagnostic);
    }
}
