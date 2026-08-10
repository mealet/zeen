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

fn compile_ok(src: &str) -> String {
    match compile(src) {
        Ok(mir) => mir,
        Err(errors) => panic!(
            "expected compilation to succeed, got errors:\n{}",
            errors.join("\n")
        ),
    }
}

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
    assert!(mir.contains("fn Vec2.add(%0: Vec2"), "MIR:\n{mir}");
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
    assert!(mir.contains("fn Box[i32].add("), "MIR:\n{mir}");
}

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

#[test]
fn unicode_char_literal_single_codepoint() {
    compile_ok("fn main() { let c = 'λ'; }");
}

#[test]
fn array_length_overflow_is_reported() {
    compile_err_contains(
        "struct A { x: [4294967296 * 4294967296]i32 }",
        "array length overflows",
    );
}

#[test]
fn unterminated_block_comment_is_reported() {
    compile_err_contains("/* never closed", "unterminated block comment");
}

#[test]
fn unused_core_functions_are_not_lowered_without_main() {
    let mir = compile_ok("");
    for core_method in [
        "fn eq(",
        "fn add(",
        "fn sub(",
        "fn display()",
        "fn debug()",
        "fn slice(",
    ] {
        assert!(
            !mir.contains(core_method),
            "unused core method `{core_method}` must not be lowered:\n{mir}"
        );
    }
}

#[test]
fn user_struct_layouts_are_printed_without_main() {
    let mir = compile_ok(
        r#"
struct Vector { x: i32, y: i32 }
struct Box[T] { value: T }
"#,
    );
    assert!(mir.contains("struct Vector { i32, i32 };"), "MIR:\n{mir}");
    assert!(mir.contains("struct Box[T] { T };"), "MIR:\n{mir}");
}

#[test]
fn user_functions_are_lowered_without_main() {
    let mir = compile_ok("fn unused() i32 { return 42; }");
    assert!(mir.contains("fn unused() i32"), "MIR:\n{mir}");
}

#[test]
fn reachable_core_method_lowers_without_main() {
    let mir = compile_ok(
        r#"
struct Vec2 { x: i32, y: i32 }
implement Add: Vec2 {
    fn add(self, other: Vec2) Vec2 { return self; }
}
fn make() Vec2 {
    let a = Vec2 { .x = 1, .y = 2 };
    let b = Vec2 { .x = 3, .y = 4 };
    return a + b;
}
"#,
    );
    assert!(mir.contains("fn Vec2.add("), "MIR:\n{mir}");
}

#[test]
fn slice_layout_is_printed_and_addr_of_array_yields_slice() {
    let mir = compile_ok(
        r#"
fn main() {
    let a: [4]i32 = [1, 2, 3, 4];
    let b: []i32 = &a;
    let _ = b[2];
}
"#,
    );
    assert!(
        mir.contains("struct []i32 { [*]i32, usize };"),
        "slice layout must be printed: MIR:\n{mir}"
    );
    assert!(
        mir.contains("slice { move %3, 4 }"),
        "&array must build a `{{ ptr, len }}` slice aggregate: MIR:\n{mir}"
    );
    assert!(
        mir.contains(".ptr["),
        "slice index must project through the slice `ptr`: MIR:\n{mir}"
    );
}
