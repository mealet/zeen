use zeen_driver::{CompilationContext, MietteDriver, PathsConfig};

use std::{cell::RefCell, collections::HashSet, path::Path, rc::Rc, sync::Arc};

include!(concat!(env!("OUT_DIR"), "/core_files.rs"));

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
                core_files: CORE_FILES.iter().map(|file| file.to_basic()).collect(),
                mode: Default::default(),
                output: Default::default(),
            };

            let content = Arc::new(content);
            let rodeo = Rc::new(RefCell::new(lasso::Rodeo::default()));
            let bump = bumpalo::Bump::default();

            let driver = MietteDriver::new();
            let mut tokens = zeen_lexer::tokenize(&content);

            let filename = Rc::new(
                Path::new(&path)
                    .file_name()
                    .unwrap_or(std::ffi::OsStr::new(&path))
                    .to_string_lossy()
                    .to_string(),
            );

            let mut parser = zeen_parser::Parser::new(
                Rc::clone(&filename),
                Arc::clone(&content),
                &mut tokens,
                &bump,
                Rc::clone(&rodeo),
            );

            let program = parser.parse_program().unwrap_or_else(|errors| {
                for err in errors {
                    let report_string = driver.report(err).unwrap();
                    eprintln!("{}", report_string);
                }

                std::process::exit(1);
            });

            let (resolved_program, mut resolution_result) = zeen_resolve::resolve(
                Rc::clone(&filename),
                Arc::clone(&content),
                Path::new(&path),
                program,
                &bump,
                Rc::clone(&rodeo),
                &mut context,
            )
            .unwrap_or_else(|errors| {
                for err in errors {
                    let report_string = driver.report(&err).unwrap();
                    eprintln!("{}", report_string);
                }

                std::process::exit(1);
            });

            let mut hir_lowering =
                zeen_hir::HirLowering::new(&resolution_result, Rc::clone(&rodeo));
            let hir_module = hir_lowering.lower_module(resolved_program);

            drop(bump);

            let mut typechecker =
                zeen_typecheck::TypeChecker::new(&mut resolution_result, Rc::clone(&rodeo));
            typechecker.check_module(&hir_module);

            let typechecker_result = typechecker.finish();

            if !typechecker_result.errors.is_empty() {
                for err in typechecker_result.errors {
                    let report_string = driver.report(&err).unwrap();
                    eprintln!("{}", report_string);
                }

                std::process::exit(1);
            }
        }
        Err(err) => {
            eprintln!("Unable to open file: {}", err);
            std::process::exit(1);
        }
    }
}
