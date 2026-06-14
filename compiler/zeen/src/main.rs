#![allow(unused)]

use zeen_driver::{CompilationContext, MietteDriver, PathsConfig};

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Mutex},
};

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
            let mut context = CompilationContext {
                paths: PathsConfig {
                    project_root: Path::new("compiler").into(),
                    std_root: Some(Path::new("compiler").into()),
                    linked: HashSet::new(),
                },
                mode: Default::default(),
                output: Default::default(),
            };

            let content = Arc::new(content);
            let rodeo = Arc::new(Mutex::new(lasso::Rodeo::default()));
            let bump = bumpalo::Bump::default();

            let driver = MietteDriver::new();
            let mut tokens = zeen_lexer::tokenize(&content);

            let filename = Arc::new(
                Path::new(&path)
                    .file_name()
                    .unwrap_or(std::ffi::OsStr::new(&path))
                    .to_string_lossy()
                    .to_string(),
            );

            let mut parser = zeen_parser::Parser::new(
                Arc::clone(&filename),
                Arc::clone(&content),
                &mut tokens,
                &bump,
                Arc::clone(&rodeo),
            );

            let program = parser.parse_program().unwrap_or_else(|errors| {
                for err in errors {
                    let report_string = driver.report(err).unwrap();
                    eprintln!("{}", report_string);
                }

                std::process::exit(1);
            });

            zeen_resolve::resolve(
                Arc::clone(&filename),
                Arc::clone(&content),
                Path::new(&path),
                program,
                &bump,
                Arc::clone(&rodeo),
                &mut context,
            );
        }
        Err(err) => {
            eprintln!("Unable to open file: {}", err);
            std::process::exit(1);
        }
    }
}
