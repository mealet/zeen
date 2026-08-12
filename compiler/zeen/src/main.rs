use clap::{CommandFactory, Parser};
use std::{
    cell::RefCell, collections::HashSet, io::Write, path::Path, process::exit, rc::Rc, sync::Arc,
};
use zeen_driver::{CompilationContext, CompilationOutput, MietteDriver, PathsConfig};

mod cli;

include!(concat!(env!("OUT_DIR"), "/core_files.rs"));

/// Appends `ext` to `path` if the file name doesn't already end with it
/// (case-insensitive). Used to give `--emit` outputs their default extension.
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

                if err.kind() == clap::error::ErrorKind::DisplayHelp {
                    exit(0);
                }

                exit(1);
            }
        }
    });

    let filename = args
        .path
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
            std::fs::canonicalize(&args.path)
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
        let src = std::fs::read_to_string(&args.path).unwrap_or_else(|err| {
            cli::println_error(format!(
                "Unable to read source ({}) file: {}",
                args.path.display(),
                err
            ));
            exit(1);
        });

        Arc::new(src)
    };

    let filename = Rc::new(filename.to_string());
    let project_root = std::fs::canonicalize(&args.path)
        .expect("already verified earlier")
        .parent()
        .expect("must work")
        .into();

    let mut context = CompilationContext {
        paths: PathsConfig {
            project_root,
            std_root: None,
            linked: HashSet::new(),
        },
        core_files: CORE_FILES.iter().map(|file| file.to_basic()).collect(),
        mode: args.mode,
        output: args.emit,
    };

    cli::println_info(
        "Setting",
        format!(
            "up project (root dir: \"{}\")",
            context.paths.project_root.display()
        ),
    );

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

    cli::println_info(
        "Resolving",
        format!("program ({} declarations)", program.len()),
    );

    let (resolved_program, mut resolution_result) = zeen_resolve::resolve(
        Rc::clone(&filename),
        Arc::clone(&content),
        Path::new(&args.path),
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
    );

    let flow_result = zeen_flow::run_dataflow(
        &mut lowered_mir.program,
        &mut typechecker_result,
        &resolution_result,
        Rc::clone(&rodeo),
    );

    match flow_result {
        Ok(result) => {
            for warning in &result.warnings {
                let report_string = driver.report(warning).unwrap();
                eprintln!("{}", report_string);
            }
            if !result.warnings.is_empty() {
                cli::println_warn(format!(
                    "Compiler reported {} warning(s)",
                    result.warnings.len()
                ));
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

        let output_path = with_default_extension(&args.output, "mir");

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
        target: None,
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
            let output_path = with_default_extension(&args.output, "ll");

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
            let output_path = with_default_extension(&args.output, "o");

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
            cli::println_warn(
                "Binary emission requires a linker which is not implemented yet; nothing was written",
            );
        }

        CompilationOutput::EmitMIR => unreachable!("handled above"),
    }
}
