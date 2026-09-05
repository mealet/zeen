use clap::{CommandFactory, Parser};
use std::{
    cell::RefCell, collections::HashSet, env, io::Write, path::Path, process::exit, rc::Rc,
    sync::Arc,
};
use zeen_driver::{CompilationContext, CompilationOutput, MietteDriver, PathsConfig};

mod cli;
mod targets;

include!(concat!(env!("OUT_DIR"), "/core_files.rs"));

fn with_default_extension(path: &Path, ext: &str) -> std::path::PathBuf {
    if path
        .extension()
        .is_some_and(|existing| existing.to_string_lossy().eq_ignore_ascii_case(ext))
    {
        return path.to_path_buf();
    }

    let mut name = path.as_os_str().to_os_string();
    name.push(".");
    name.push(ext);
    std::path::PathBuf::from(name)
}

/// Resolves the std library root directory: the `--std` flag, then the
/// `ZEEN_STD` environment variable, then the default `~/.zeen/std`
/// installation. An explicitly configured path must exist; when nothing is
/// configured the compiler keeps running and reports on `use std.*` usage
/// instead.
fn resolve_std_root(explicit: Option<&Path>) -> Result<Option<std::path::PathBuf>, String> {
    const REPO: &str = "https://github.com/mealet/zeen";

    if let Some(path) = explicit {
        return if path.is_dir() {
            Ok(Some(path.to_path_buf()))
        } else {
            Err(format!(
                "std library directory not found at `{}` (set by --std); install the std library, see {REPO}",
                path.display()
            ))
        };
    }

    if let Some(env_path) = env::var_os("ZEEN_STD") {
        let path = std::path::PathBuf::from(env_path);
        return if path.is_dir() {
            Ok(Some(path))
        } else {
            Err(format!(
                "`ZEEN_STD` points to `{}` which does not exist or is not a directory; see {REPO}",
                path.display()
            ))
        };
    }

    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"));
    Ok(home
        .map(|home| Path::new(&home).join(".zeen").join("std"))
        .filter(|path| path.is_dir()))
}

fn main() {
    let args = cli::Args::try_parse().unwrap_or_else(|err| {
        let mut command = cli::Args::command();

        let authors_env = env!("CARGO_PKG_AUTHORS");
        let authors_fmt = if authors_env.contains(":") {
            format!("\n| {}", authors_env.replace(":", "\n| "))
        } else {
            authors_env.to_owned()
        };

        cli::println_primary("💤 Zeen Programming Language");
        cli::println_basic(format!("| - version: {}", env!("CARGO_PKG_VERSION")));
        cli::println_basic(format!("| - authors: {}", authors_fmt));

        match err.kind() {
            clap::error::ErrorKind::DisplayVersion => {
                exit(0);
            }

            _ => {
                cli::println_basic("");
                cli::println_primary("🍀 Options:");

                command.print_help().unwrap();

                cli::println_basic("");
                cli::println_primary("🎓 Examples of usage:");
                cli::println_basic("  zeen example.zn output");
                cli::println_basic("  zeen example.zn output -m Release");
                cli::println_basic("  zeen example.zn output --emit IR");
                cli::println_basic("  zeen example.zn output --target x86_64-pc-windows-gnu");
                cli::println_basic("  zeen --targets-list");

                if err.kind() == clap::error::ErrorKind::DisplayHelp {
                    exit(0);
                }

                exit(1);
            }
        }
    });

    if let Some(triple) = &args.target
        && !targets::is_supported(triple)
    {
        cli::println_error(format!("unsupported target triple `{triple}`"));
        cli::println_basic("\nSupported targets:");
        for supported in targets::SUPPORTED_TARGETS {
            cli::println_basic(format!("  {supported}"));
        }
        exit(1);
    }

    if args.targets_list {
        cli::println_primary(format!(
            "Supported targets ({}):",
            targets::SUPPORTED_TARGETS.len()
        ));

        for triple in targets::SUPPORTED_TARGETS {
            cli::println_basic(format!("  {triple}"));
        }

        exit(0);
    }

    let target_triple = args.target.clone().unwrap_or_else(targets::host_target);

    let path = args
        .path
        .expect("path is required unless `--targets-list` is given");
    let output = args
        .output
        .expect("output is required unless `--targets-list` is given");

    let filename = path
        .file_name()
        .unwrap_or_else(|| {
            cli::println_error("Unable to get source filename");
            exit(1);
        })
        .to_str()
        .unwrap_or_else(|| {
            cli::println_error("Unable to get source filename");
            exit(1);
        });

    cli::println_info(
        "Reading",
        format!(
            "`{}` ({})",
            filename,
            std::fs::canonicalize(&path)
                .unwrap_or_else(|_| {
                    cli::println_error(format!("File `{}` doesn't exist", filename));
                    exit(1);
                })
                .display()
        ),
    );

    let rodeo = Rc::new(RefCell::new(lasso::Rodeo::default()));
    let bump = bumpalo::Bump::default();
    let driver = MietteDriver::new();

    let content = {
        let src = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            cli::println_error(format!(
                "Unable to read source ({}) file: {}",
                path.display(),
                err
            ));
            exit(1);
        });

        Arc::new(src)
    };

    let filename = Rc::new(filename.to_string());
    let project_root = std::fs::canonicalize(&path)
        .expect("already verified earlier")
        .parent()
        .expect("must work")
        .into();

    let std_root = match resolve_std_root(args.std.as_deref()) {
        Ok(path) => path,
        Err(message) => {
            cli::println_error(message);
            exit(1);
        }
    };

    let mut context = CompilationContext {
        paths: PathsConfig {
            project_root,
            std_root,
            linked: HashSet::new(),
        },
        core_files: CORE_FILES.iter().map(|file| file.to_basic()).collect(),
        mode: args.mode,
        output: args.emit,
        target: Some(target_triple.clone()),
    };

    cli::println_info(
        "Setting",
        format!(
            "up project (root dir: \"{}\")",
            context.paths.project_root.display()
        ),
    );

    cli::println_info("Target", format!("triple `{target_triple}`"));

    let mut tokens = zeen_lexer::tokenize(&content);
    let mut parser = zeen_parser::Parser::new(
        Rc::clone(&filename),
        Arc::clone(&content),
        &mut tokens,
        &bump,
        Rc::clone(&rodeo),
    );

    cli::println_info("Parsing", "abstract syntax tree");

    let program = parser.parse_program().unwrap_or_else(|errors| {
        for err in errors {
            let report_string = driver.report(err).unwrap();
            eprintln!("{}", report_string);
        }

        cli::println_error(format!("Compiler returned {} error(s)", errors.len()));

        exit(1);
    });

    cli::println_info("Preprocessing", "conditional declarations for target");

    let target = zeen_driver::Target::parse(&target_triple);
    let program = zeen_preprocessor::resolve(program, &bump, &rodeo, &target, context.mode);

    cli::println_info(
        "Resolving",
        format!("program ({} declarations)", program.len()),
    );

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
        for err in &errors {
            let report_string = driver.report(err).unwrap();
            eprintln!("{}", report_string);
        }

        cli::println_error(format!("Compiler returned {} errors", errors.len()));

        exit(1);
    });

    let mut hir_lowering = zeen_hir::HirLowering::new(&resolution_result, Rc::clone(&rodeo));
    let hir_module = hir_lowering.lower_module(resolved_program);

    drop(bump);

    cli::println_info(
        "Checking",
        format!(
            "resolved program ({} definitions)",
            resolution_result.defs.len()
        ),
    );

    let mut typechecker =
        zeen_typecheck::TypeChecker::new(&mut resolution_result, &context, Rc::clone(&rodeo));
    typechecker.check_module(&hir_module);

    let mut typechecker_result = typechecker.finish().unwrap_or_else(|errors| {
        for err in &errors {
            let report_string = driver.report(err).unwrap();
            eprintln!("{}", report_string);
        }

        cli::println_error(format!("Compiler returned {} error(s)", errors.len()));

        exit(1);
    });

    let mut lowered_mir = zeen_mir::lowering::lower_program(
        Rc::clone(&rodeo),
        &mut typechecker_result,
        &resolution_result,
        &hir_module,
        context.mode,
    )
    .unwrap_or_else(|errors| {
        for err in &errors {
            let report_string = driver.report(err).unwrap();
            eprintln!("{}", report_string);
        }

        cli::println_error(format!("Compiler returned {} error(s)", errors.len()));

        exit(1);
    });

    let flow_result = zeen_flow::run_dataflow(
        &mut lowered_mir.program,
        &mut typechecker_result,
        &resolution_result,
        Rc::clone(&rodeo),
    );

    match flow_result {
        Ok(result) => {
            if !args.no_warns {
                let mut warnings = Vec::new();
                warnings.extend(
                    lowered_mir
                        .warnings
                        .iter()
                        .map(|w| w as &dyn miette::Diagnostic),
                );
                warnings.extend(result.warnings.iter().map(|w| w as &dyn miette::Diagnostic));

                let count = warnings.len();
                for warning in warnings {
                    let report_string = driver.report(warning).unwrap();
                    eprintln!("{}", report_string);
                }

                if count > 0 {
                    cli::println_warn(format!("Compiler reported {count} warning(s)"));
                }
            }
        }
        Err(errors) => {
            for err in &errors {
                let report_string = driver.report(err).unwrap();
                eprintln!("{}", report_string);
            }

            cli::println_error(format!("Compiler returned {} error(s)", errors.len()));

            exit(1);
        }
    }

    if args.emit == CompilationOutput::EmitMIR {
        let printed_mir = zeen_mir::printer::print_mir_program(
            &lowered_mir.program,
            &typechecker_result,
            &resolution_result,
            &rodeo,
        );

        let output_path = with_default_extension(&output, "mir");

        let mut output_file = std::fs::File::create(&output_path).unwrap_or_else(|_| {
            cli::println_warn("Unable to write MIR to file, printing to stdout...");
            println!("\n{}", printed_mir);

            exit(0);
        });

        output_file
            .write_all(printed_mir.as_bytes())
            .unwrap_or_else(|_| {
                cli::println_warn("Unable to write MIR to file, printing to stdout...");
                println!("\n{}", printed_mir);

                exit(0);
            });

        cli::println_info(
            "Emitted",
            format!("MIR representation to the file ({})", output_path.display()),
        );

        exit(0);
    }

    cli::println_info("Generating", "LLVM IR from MIR");

    let codegen_options = zeen_codegen_llvm::CodegenOptions {
        mode: context.mode,
        target: Some(target_triple.clone()),
        main_fn: lowered_mir.main_fn,
        source_file_name: filename.to_string(),
    };

    let context = inkwell::context::Context::create();

    let mut codegen = zeen_codegen_llvm::CodeGen::new(
        &context,
        &lowered_mir.program,
        &typechecker_result,
        &resolution_result,
        Rc::clone(&rodeo),
        codegen_options,
    )
    .unwrap_or_else(|err| {
        let report_string = driver.report(&err).unwrap();
        eprintln!("{}", report_string);
        cli::println_error("Codegen failed");
        exit(1);
    });

    if let Err(err) = codegen.generate() {
        let report_string = driver.report(&err).unwrap();
        eprintln!("{}", report_string);
        cli::println_error("Codegen failed");
        exit(1);
    }

    if let Err(err) = codegen.verify() {
        let report_string = driver.report(&err).unwrap();
        eprintln!("{}", report_string);
        cli::println_error("Codegen failed");
        exit(1);
    }

    match args.emit {
        CompilationOutput::EmitIR => {
            let output_path = with_default_extension(&output, "ll");

            if let Err(err) = codegen.emit_ir(&output_path) {
                let report_string = driver.report(&err).unwrap();
                eprintln!("{}", report_string);
                cli::println_error("Codegen failed");
                exit(1);
            }

            cli::println_info(
                "Emitted",
                format!("LLVM IR to the file ({})", output_path.display()),
            );
        }

        CompilationOutput::Object => {
            let output_path = with_default_extension(
                &output,
                zeen_linker::linker::ObjectLinker::object_extension_for(&target_triple),
            );

            if let Err(err) = codegen.emit_object(&output_path) {
                let report_string = driver.report(&err).unwrap();
                eprintln!("{}", report_string);
                cli::println_error("Codegen failed");
                exit(1);
            }

            cli::println_info(
                "Emitted",
                format!("object file to the file ({})", output_path.display()),
            );
        }

        CompilationOutput::Binary => {
            let linker =
                zeen_linker::linker::ObjectLinker::detect(&target_triple).unwrap_or_else(|err| {
                    cli::println_error(err);
                    exit(1);
                });

            let object_path = std::env::temp_dir().join(format!(
                "zeen-{}.{}",
                std::process::id(),
                linker.object_extension()
            ));

            if let Err(err) = codegen.emit_object(&object_path) {
                let report_string = driver.report(&err).unwrap();
                eprintln!("{}", report_string);
                cli::println_error("Codegen failed");
                exit(1);
            }

            let result = linker.link(std::slice::from_ref(&object_path), &output, &[]);

            std::fs::remove_file(&object_path).ok();

            match result {
                Ok(output_path) => cli::println_info(
                    "Emitted",
                    format!(
                        "binary (with {}): `{}`",
                        linker.name(),
                        output_path.display()
                    ),
                ),
                Err(err) => {
                    cli::println_error(format!(
                        "Linker failed (object linker: `{}`)",
                        linker.name()
                    ));
                    eprintln!("\n{err}\n");
                    exit(1);
                }
            }
        }

        CompilationOutput::EmitMIR => unreachable!("handled above"),
    }
}
