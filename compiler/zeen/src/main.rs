#![allow(unused)]

use zeen_driver::MietteDriver;

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
#[error("test error")]
struct TestError {
    #[source_code]
    src: NamedSource<String>,
    #[label("here")]
    span: SourceSpan,
}

fn main() {
    const SRC: &str = "/* */ 123 hello 123";

    let tokens = zeen_lexer::tokenize(SRC);

    for tok in tokens {
        println!("{:?}", tok);
    }
}
