use std::{
    cell::RefCell,
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use bumpalo::Bump;
use lasso::Rodeo;
use zeen_driver::{CompilationContext, CompilationMode, CompilationOutput, PathsConfig};
use zeen_flow::{FlowResult, run_dataflow};
use zeen_hir::HirLowering;
use zeen_mir::lowering::lower_program;
use zeen_parser::Parser;
use zeen_typecheck::TypeChecker;

fn core_files() -> Vec<(&'static str, &'static str)> {
    let core_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../lib/core")
        .canonicalize()
        .expect("lib/core must exist");

    let mut files: Vec<(&'static str, &'static str)> = Vec::new();
    for entry in fs::read_dir(&core_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("zn") {
            continue;
        }
        let stem = path.file_stem().unwrap().to_str().unwrap();
        let name: &'static str = Box::leak(format!("core.{stem}").into_boxed_str());
        let value: &'static str = Box::leak(fs::read_to_string(&path).unwrap().into_boxed_str());
        files.push((name, value));
    }
    files
}

fn run(src: &str) -> Result<FlowResult, Vec<String>> {
    let rodeo = Rc::new(RefCell::new(Rodeo::default()));
    let bump = Bump::default();
    let content = Arc::new(src.to_string());
    let filename = Rc::new("test.zn".to_string());

    let mut context = CompilationContext {
        paths: PathsConfig {
            project_root: PathBuf::from("/"),
            std_root: None,
            linked: HashSet::new(),
        },
        core_files: core_files(),
        mode: CompilationMode::Debug,
        output: CompilationOutput::EmitMIR,
    };

    let mut tokens = zeen_lexer::tokenize(&content);
    let mut parser = Parser::new(
        Rc::clone(&filename),
        Arc::clone(&content),
        &mut tokens,
        &bump,
        Rc::clone(&rodeo),
    );
    let program = parser.parse_program().map_err(|errors| {
        errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<String>>()
    })?;

    let (resolved_program, mut resolution_result) = zeen_resolve::resolve(
        Rc::clone(&filename),
        Arc::clone(&content),
        Path::new("/test.zn"),
        program,
        &bump,
        Rc::clone(&rodeo),
        &mut context,
    )
    .map_err(|errors| {
        errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<String>>()
    })?;

    let mut hir_lowering = HirLowering::new(&resolution_result, Rc::clone(&rodeo));
    let hir_module = hir_lowering.lower_module(resolved_program);

    drop(bump);

    let mut typechecker = TypeChecker::new(&mut resolution_result, &context, Rc::clone(&rodeo));
    typechecker.check_module(&hir_module);
    let mut typecheck_result = typechecker.finish().map_err(|errors| {
        errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<String>>()
    })?;

    let mut lowered = lower_program(
        Rc::clone(&rodeo),
        &mut typecheck_result,
        &resolution_result,
        &hir_module,
    );

    run_dataflow(
        &mut lowered.program,
        &typecheck_result,
        &resolution_result,
        Rc::clone(&rodeo),
    )
    .map_err(|errors| errors.iter().map(|e| format!("{e:?}")).collect())
}

fn flow_ok(src: &str) -> FlowResult {
    match run(src) {
        Ok(result) => result,
        Err(errors) => panic!("expected dataflow to pass, got errors:\n{errors:?}"),
    }
}

fn flow_err(src: &str) -> Vec<String> {
    match run(src) {
        Ok(_) => panic!("expected dataflow to fail, but it passed"),
        Err(errors) => errors,
    }
}

#[test]
fn simple_copy_arithmetic_passes() {
    flow_ok(
        r#"
fn main() {
    let a = 1;
    let b = a + 2;
}
"#,
    );
}

#[test]
fn whole_move_of_move_only_struct_passes() {
    flow_ok(
        r#"
struct Pair { pub a: i32, pub b: i32 }
fn main() {
    let p = Pair { .a = 1, .b = 2 };
    let q = p;
}
"#,
    );
}

#[test]
fn partial_move_of_non_drop_struct_passes() {
    flow_ok(
        r#"
struct Pair { pub a: i32, pub b: i32 }
fn main() {
    let p = Pair { .a = 1, .b = 2 };
    let a = p.a;
    let b = p.b;
}
"#,
    );
}

#[test]
fn use_after_move_is_error() {
    let errors = flow_err(
        r#"
struct Inner { pub x: i32 }
struct Pair { pub a: Inner, pub b: Inner }
fn main() {
    let p = Pair { .a = Inner { .x = 1 }, .b = Inner { .x = 2 } };
    let q = p;
    let r = p;
}
"#,
    );
    assert!(!errors.is_empty());
}

#[test]
fn reinitialized_struct_is_usable_again() {
    flow_ok(
        r#"
struct Inner { pub x: i32 }
struct Pair { pub a: Inner, pub b: Inner }
fn main() {
    let p = Pair { .a = Inner { .x = 1 }, .b = Inner { .x = 2 } };
    let a = p.a;
    p.a = Inner { .x = 3 };
    let b = p;
}
"#,
    );
}

#[test]
fn use_of_partially_moved_struct_is_error() {
    let errors = flow_err(
        r#"
struct Inner { pub x: i32 }
struct Pair { pub a: Inner, pub b: Inner }
fn main() {
    let p = Pair { .a = Inner { .x = 1 }, .b = Inner { .x = 2 } };
    let a = p.a;
    let b = p;
}
"#,
    );
    assert!(!errors.is_empty());
}

#[test]
fn move_out_of_drop_struct_field_is_error() {
    let errors = flow_err(
        r#"
struct Inner { pub x: i32 }
struct Buffer { pub data: Inner }
implement Drop : Buffer {
    fn drop(self) void {}
}
fn main() {
    let b = Buffer { .data = Inner { .x = 1 } };
    let x = b.data;
}
"#,
    );
    assert!(!errors.is_empty());
}

#[test]
fn use_after_move_across_call_is_error() {
    let errors = flow_err(
        r#"
struct Inner { pub x: i32 }
struct Pair { pub a: Inner, pub b: Inner }
fn take(p: Pair) {}
fn main() {
    let p = Pair { .a = Inner { .x = 1 }, .b = Inner { .x = 2 } };
    take(p);
    take(p);
}
"#,
    );
    assert!(!errors.is_empty());
}
