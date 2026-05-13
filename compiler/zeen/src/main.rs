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
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("No path to file found");

        std::process::exit(1);
    });

    match std::fs::read_to_string(&path) {
        Ok(content) => {
            let driver = MietteDriver::new();
            let tokens = zeen_lexer::tokenize(&content);

            let start = std::time::Instant::now();

            for token in tokens {
                if token.kind == zeen_lexer::TokenKind::Unknown {
                    let rep = driver
                        .report(&SrcDebugger {
                            dbg: format!("Unknown token"),
                            src: NamedSource::new(&path, content.clone()),
                            span: token.span,
                        })
                        .unwrap();

                    eprintln!("{}", rep);
                    std::process::exit(1);
                }
            }
        }
        Err(err) => {
            eprintln!("Unable to open file: {}", err);
            std::process::exit(1);
        }
    }
}
