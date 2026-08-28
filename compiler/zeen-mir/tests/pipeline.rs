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

fn compile_mode(src: &str, mode: CompilationMode) -> Result<String, Vec<String>> {
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
        mode,
        output: CompilationOutput::EmitMIR,
        target: None,
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
        mode,
    )
    .unwrap();

    Ok(zeen_mir::printer::print_mir_program(
        &lowered.program,
        &typecheck_result,
        &resolution_result,
        &rodeo,
    ))
}

fn compile(src: &str) -> Result<String, Vec<String>> {
    compile_mode(src, CompilationMode::Debug)
}

fn compile_mode_ok(src: &str, mode: CompilationMode) -> String {
    match compile_mode(src, mode) {
        Ok(mir) => mir,
        Err(errors) => panic!(
            "expected compilation to succeed, got errors:\n{}",
            errors.join("\n")
        ),
    }
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

#[test]
fn slice_ptr_and_len_fields_are_accessible() {
    let mir = compile_ok(
        r#"
fn main() {
    let a: [4]i32 = [1, 2, 3, 4];
    let b: []i32 = &a;

    let slice_ptr: [*]i32 = b.ptr;
    let slice_len: usize = b.len;
}
"#,
    );
    assert!(
        mir.contains("= %2.ptr;"),
        "`b.ptr` must project through the slice `ptr` field: MIR:\n{mir}"
    );
    assert!(
        mir.contains("= %2.len;"),
        "`b.len` must project through the slice `len` field: MIR:\n{mir}"
    );
}

#[test]
fn array_len_is_a_compile_time_constant() {
    let mir = compile_ok(
        r#"
fn main() {
    let a: [4]i32 = [1, 2, 3, 4];
    let n: usize = a.len;
}
"#,
    );
    assert!(
        mir.contains("= 4;"),
        "`a.len` must lower to the constant array length: MIR:\n{mir}"
    );
}

#[test]
fn dbg_macro_is_kept_in_debug_mode() {
    let mir = compile_ok(
        r#"
fn main() {
    let a: [4]i32 = [1, 2, 3, 4];
    let i: usize = 1;
    let x = @dbg(a[i]);
}
"#,
    );
    assert!(
        mir.contains("@dbg("),
        "@dbg must stay in the MIR in Debug mode: MIR:\n{mir}"
    );
}

#[test]
fn dbg_macro_is_elided_in_release_mode() {
    let mir = compile_mode_ok(
        r#"
fn main() {
    let a: [4]i32 = [1, 2, 3, 4];
    let i: usize = 1;
    let x = @dbg(a[i]);
}
"#,
        CompilationMode::Release,
    );
    assert!(
        !mir.contains("@dbg"),
        "@dbg must be removed from the MIR in Release mode, leaving the plain expression: MIR:\n{mir}"
    );
    assert!(
        mir.contains("= %0[%2];"),
        "the @dbg argument must still be evaluated and assigned: MIR:\n{mir}"
    );
}

#[test]
fn array_index_is_bounds_checked_in_debug_mode() {
    let mir = compile_ok(
        r#"
fn main() {
    let a: [4]i32 = [1, 2, 3, 4];
    let i: usize = 1;
    let x = a[i];
}
"#,
    );
    assert!(
        mir.contains("switchInt"),
        "array index must be bounds-checked in Debug mode: MIR:\n{mir}"
    );
    assert!(
        mir.contains("@panic"),
        "out-of-bounds access must diverge into a panic in Debug mode: MIR:\n{mir}"
    );
}

#[test]
fn array_index_is_not_bounds_checked_in_release_mode() {
    let mir = compile_mode_ok(
        r#"
fn main() {
    let a: [4]i32 = [1, 2, 3, 4];
    let i: usize = 1;
    let x = a[i];
}
"#,
        CompilationMode::Release,
    );
    assert!(
        !mir.contains("switchInt"),
        "array index must not be bounds-checked in Release mode: MIR:\n{mir}"
    );
    assert!(
        !mir.contains("@panic"),
        "no panic must be emitted for indexing in Release mode: MIR:\n{mir}"
    );
}

#[test]
fn slice_index_is_bounds_checked_against_runtime_len_in_debug_mode() {
    let mir = compile_ok(
        r#"
fn main() {
    let a: [4]i32 = [1, 2, 3, 4];
    let b: []i32 = &a;
    let i: usize = 1;
    let x = b[i];
}
"#,
    );
    assert!(
        mir.contains(".len)"),
        "slice index must be bounds-checked against the runtime `.len`: MIR:\n{mir}"
    );
    assert!(
        mir.contains("@panic"),
        "out-of-bounds slice access must diverge into a panic: MIR:\n{mir}"
    );
}

#[test]
fn nested_fn_is_printed_with_parent_prefixed_name() {
    let mir = compile_ok(
        r#"
fn main() {
    fn foo() void {
        @println("hi");
    }
    foo();
}
"#,
    );
    assert!(
        mir.contains("fn main->foo() void"),
        "nested fn must be printed with its parent prefix: MIR:\n{mir}"
    );
}

#[test]
fn deeply_nested_fn_is_printed_with_full_parent_chain() {
    let mir = compile_ok(
        r#"
fn main() {
    fn inner() void {
        fn deepest() void {
            @println("deep");
        }
        deepest();
    }
    inner();
}
"#,
    );
    assert!(
        mir.contains("fn main->inner->deepest() void"),
        "nested fn must be printed with the full parent chain: MIR:\n{mir}"
    );
}

#[test]
fn nested_fn_is_visible_from_declaration_point_onwards() {
    compile_ok(
        r#"
fn main() {
    fn foo() void { @println("hi"); }
    foo();
}
"#,
    );
}

#[test]
fn nested_fn_is_not_visible_before_its_declaration() {
    compile_err_contains(
        r#"
fn main() {
    foo();
    fn foo() void { @println("hi"); }
}
"#,
        "unresolved identifier",
    );
}

#[test]
fn nested_fn_is_not_visible_outside_parent() {
    compile_err_contains(
        r#"
fn main() {
    fn foo() void { @println("hi"); }
}
fn bar() void {
    foo();
}
"#,
        "unresolved identifier",
    );
}

#[test]
fn nested_fn_cannot_capture_enclosing_local() {
    compile_err_contains(
        r#"
fn main() {
    let x: i32 = 5;
fn foo() void { @println(x); }
    foo();
}
"#,
        "nested function cannot capture",
    );
}

#[test]
fn nested_fn_cannot_capture_enclosing_generic() {
    compile_err_contains(
        r#"
fn wrapper[T](x: T) T {
    fn inner(y: T) T { return y; }
    return inner(x);
}
"#,
        "nested function cannot capture",
    );
}

#[test]
fn nested_fn_cannot_be_pub() {
    compile_err_contains(
        r#"
fn main() {
    pub fn foo() void { @println("hi"); }
    foo();
}
"#,
        "nested functions cannot be `public`",
    );
}

#[test]
fn nested_fn_named_main_is_not_the_entry_point() {
    let mir = compile_ok(
        r#"
fn main() {
    fn main() void { @println("inner"); }
    main();
}
"#,
    );
    assert!(
        mir.contains("fn main() void"),
        "the top-level entry point must still exist: MIR:\n{mir}"
    );
    assert!(
        mir.contains("fn main->main() void"),
        "nested `main` must be parent-prefixed, not replace the entry point: MIR:\n{mir}"
    );
}

#[test]
fn nested_fn_can_recurse() {
    let mir = compile_ok(
        r#"
fn main() {
    fn fib(n: i32) i32 {
        if (n < 2) { return n; }
        return fib(n - 1) + fib(n - 2);
    }
    let a = fib(10);
    @println("{}", a);
}
"#,
    );
    assert!(
        mir.contains("fn main->fib(%0: i32)"),
        "recursive nested fn must be registered under the parent prefix: MIR:\n{mir}"
    );
}

#[test]
fn nested_fn_can_call_sibling() {
    let mir = compile_ok(
        r#"
fn main() {
    fn helper(x: i32) i32 { return x * 2; }
    fn caller(x: i32) i32 { return helper(x); }
    let a = caller(21);
    @println("{}", a);
}
"#,
    );
    assert!(
        mir.contains("fn main->helper(%0: i32)"),
        "sibling nested fn must be registered: MIR:\n{mir}"
    );
    assert!(
        mir.contains("fn main->caller(%0: i32)"),
        "caller nested fn must be registered: MIR:\n{mir}"
    );
}

#[test]
fn float_literal_division_by_zero_compiles_in_debug_mode() {
    let mir = compile_ok(
        r#"
fn main() {
    let a: f64 = 10.0;
    let c = a / 2.0;
    let d = a / 0.0;
    @println("{} {}", c, d);
}
"#,
    );
    assert!(
        !mir.contains("@panic"),
        "float literal division must not be guarded (IEEE-754 inf/nan): MIR:\n{mir}"
    );
}

#[test]
fn void_function_ending_in_call_returns_void_constant() {
    let mir = compile_ok(
        r#"
fn bar() void { @println("b"); }
fn main() { bar() }
"#,
    );
    assert!(
        mir.contains("return void;"),
        "a void fn ending in a call must return `void`, not a void temp: MIR:\n{mir}"
    );
    assert!(
        !mir.contains("return %0;"),
        "no void temporary may be returned (codegen allocates none): MIR:\n{mir}"
    );
}

#[test]
fn void_function_with_explicit_void_return_call_lowers() {
    let mir = compile_ok(
        r#"
fn bar() void { @println("b"); }
fn main() void { return bar(); }
"#,
    );
    assert!(
        mir.contains("return void;"),
        "explicit `return bar();` must return `void`: MIR:\n{mir}"
    );
    assert!(
        !mir.contains("return %0;"),
        "no void temporary may be returned: MIR:\n{mir}"
    );
}

#[test]
fn integer_division_by_zero_is_checked_in_debug_mode() {
    let mir = compile_ok(
        r#"
fn main() {
    let a: i32 = 10;
    let b: i32 = 0;
    let c = a / b;
    @println("{}", c);
}
"#,
    );
    assert!(
        mir.contains("switchInt"),
        "integer division must be guarded in Debug mode: MIR:\n{mir}"
    );
    assert!(
        mir.contains("@panic"),
        "zero divisor must diverge into a panic in Debug mode: MIR:\n{mir}"
    );
}

#[test]
fn integer_modulo_by_zero_is_checked_in_debug_mode() {
    let mir = compile_ok(
        r#"
fn main() {
    let a: i32 = 10;
    let b: i32 = 0;
    let c = a % b;
    @println("{}", c);
}
"#,
    );
    assert!(
        mir.contains("switchInt"),
        "integer modulo must be guarded in Debug mode: MIR:\n{mir}"
    );
    assert!(
        mir.contains("@panic"),
        "zero divisor must diverge into a panic in Debug mode: MIR:\n{mir}"
    );
}

#[test]
fn integer_division_is_not_checked_in_release_mode() {
    let mir = compile_mode_ok(
        r#"
fn main() {
    let a: i32 = 10;
    let b: i32 = 0;
    let c = a / b;
    @println("{}", c);
}
"#,
        CompilationMode::Release,
    );
    assert!(
        !mir.contains("switchInt"),
        "integer division must not be guarded in Release mode: MIR:\n{mir}"
    );
    assert!(
        !mir.contains("@panic"),
        "no panic must be emitted for division in Release mode: MIR:\n{mir}"
    );
}

#[test]
fn float_division_by_zero_is_not_checked() {
    let mir = compile_ok(
        r#"
fn main() {
    let a: f64 = 10.0;
    let b: f64 = 0.0;
    let c = a / b;
    @println("{}", c);
}
"#,
    );
    assert!(
        !mir.contains("@panic"),
        "float division by zero must not panic (IEEE-754 inf/nan): MIR:\n{mir}"
    );
}
