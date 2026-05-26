#![allow(unused)]

use zeen_driver::MietteDriver;

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

use std::sync::{Arc, Mutex};

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
            let content = Arc::new(content);
            let rodeo = Arc::new(Mutex::new(lasso::Rodeo::default()));
            let bump = bumpalo::Bump::default();

            let driver = MietteDriver::new();
            let mut tokens = zeen_lexer::tokenize(&content);

            let mut parser = zeen_parser::Parser::new(
                &path,
                std::sync::Arc::clone(&content),
                &mut tokens,
                &bump,
                std::sync::Arc::clone(&rodeo),
            );

            let program = parser.parse_program().unwrap_or_else(|errors| {
                for err in errors {
                    let report_string = driver.report(err).unwrap();
                    eprintln!("{}", report_string);
                }

                std::process::exit(1);
            });

            // println!("{:#?}", program);
        }
        Err(err) => {
            eprintln!("Unable to open file: {}", err);
            std::process::exit(1);
        }
    }
}
