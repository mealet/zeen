//! End-to-end pipeline tests: source text -> MIR.
//!
//! These wire the whole compiler (lexer -> parser -> resolve -> hir -> typecheck
//! -> mir) exactly like `zeen/src/main.rs` and assert either that a program
//! compiles successfully (and produces expected MIR) or that it fails with an
//! expected diagnostic message. They cover behavior that only becomes visible
//! after typechecking/lowering, where per-crate unit tests cannot reach.

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
use zeen_hir::HirLowering;
use zeen_mir::lowering::lower_program;
use zeen_parser::Parser;
use zeen_typecheck::TypeChecker;

/// All `.zn` files under `lib/core`, embedded like `zeen/build.rs` does.
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

/// Runs a single source string through the entire compiler pipeline.
///
/// Returns `Ok(printed_mir)` on success or `Err(all_error_messages)` on the
/// first stage that fails (parse, resolve or typecheck).
fn compile(src: &str) -> Result<String, Vec<String>> {
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

    let lowered = lower_program(
        Rc::clone(&rodeo),
        &mut typecheck_result,
        &resolution_result,
        &hir_module,
    );

    Ok(zeen_mir::printer::print_mir_program(
        &lowered.program,
        &typecheck_result,
        &resolution_result,
        &rodeo,
    ))
}

/// Asserts a program compiles and returns the printed MIR.
fn compile_ok(src: &str) -> String {
    match compile(src) {
        Ok(mir) => mir,
        Err(errors) => panic!(
            "expected compilation to succeed, got errors:\n{}",
            errors.join("\n")
        ),
    }
}

/// Asserts a program fails to compile and that some error message contains
/// the given substring.
fn compile_err_contains(src: &str, needle: &str) {
    match compile(src) {
        Ok(_) => panic!("expected compilation to fail with `{needle}`, but it succeeded"),
        Err(errors) => {
            let joined = errors.join("\n");
            assert!(
                joined.contains(needle),
                "expected error containing `{needle}`, got:\n{joined}"
            );
        }
    }
}

// --> Generic monomorphization
#[test]
fn generic_struct_and_fn_instantiate() {
    let mir = compile_ok(
        r#"
struct Box[T] { value: T }
fn map2[T](b: T) T { return b; }
fn main() {
    let a = Box { .value = 1 };
    let b = map2(a);
}
"#,
    );
    assert!(mir.contains("fn map2[Box[i32]]"), "MIR:\n{mir}");
}

#[test]
fn generic_fn_over_int_instantiates() {
    let mir = compile_ok(
        r#"
fn id[T](x: T) T { return x; }
fn main() {
    let a = id(10);
}
"#,
    );
    assert!(mir.contains("fn id[i32]"), "MIR:\n{mir}");
}

// --> Operator overloading: compound assignment dispatch and implement bindings
#[test]
fn compound_assign_dispatches_overloaded_operator() {
    let mir = compile_ok(
        r#"
struct Vec2 { x: i32, y: i32 }
implement Add: Vec2 {
    fn add(self, other: Vec2) Vec2 {
        return Vec2 { .x = self.x + other.x, .y = self.y + other.y };
    }
}
fn main() {
    let a = Vec2 { .x = 1, .y = 2 };
    a += Vec2 { .x = 3, .y = 4 };
}
"#,
    );
    assert!(mir.contains("fn add(%0: Vec2"), "MIR:\n{mir}");
}

#[test]
fn generic_implement_binding_lowers() {
    let mir = compile_ok(
        r#"
struct Box[T] { value: T }
implement[U] Add: Box[U] {
    fn add(self, other: Box[U]) Box[U] { return self; }
}
fn main() {
    let a = Box { .value = 1 };
    let b = a + a;
}
"#,
    );
    assert!(mir.contains("fn add[i32]"), "MIR:\n{mir}");
}

// --> Bitwise shift binds the builtin pipeline interfaces (BitShl/BitShr)
#[test]
fn shift_left_on_u64_compiles() {
    compile_ok(
        r#"
fn main() {
    let a: u64 = 1;
    let b = a << 2;
}
"#,
    );
}

// --> Unicode char literals (char-count based parsing)
#[test]
fn unicode_char_literal_single_codepoint() {
    compile_ok("fn main() { let c = 'λ'; }");
}

// --> Array length const-eval overflow is reported, not UB
#[test]
fn array_length_overflow_is_reported() {
    compile_err_contains(
        "struct A { x: [4294967296 * 4294967296]i32 }",
        "array length overflows",
    );
}

// --> Unterminated literals/comment must produce a diagnostic, not a panic
#[test]
fn unterminated_block_comment_is_reported() {
    compile_err_contains("/* never closed", "unterminated block comment");
}
