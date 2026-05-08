#![allow(unused)]

use zeen_driver::MietteDriver;

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
#[error("debug")]
#[diagnostic(severity(Advice))]
struct SrcDebugger {
    #[source_code]
    src: NamedSource<String>,
    #[label("here")]
    span: SourceSpan,
}

fn main() {
    const SRC: &str = "/* */ hello";

    let tokens = zeen_lexer::tokenize(SRC);

    let driver = MietteDriver::new();

    for tok in tokens {
        let diagnostic = driver
            .report(&SrcDebugger {
                src: NamedSource::new("debug", SRC.into()),
                span: tok.span,
            })
            .unwrap();

        println!("{}", diagnostic);
    }
}
