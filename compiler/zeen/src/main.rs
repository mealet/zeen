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
    const SRC: &str = "123 hello 123";

    let err = TestError {
        src: NamedSource::new("test.zn", SRC.into()),
        span: SourceSpan::new(4.into(), 5),
    };

    let driver = MietteDriver::new();

    println!("{}", driver.report(&err).unwrap());
}
