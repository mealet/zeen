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
use zeen_flow::{FlowError, FlowResult, run_dataflow};
use zeen_hir::HirLowering;
use zeen_mir::MirProgram;
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

struct Compiled {
    program: MirProgram,
    typecheck: zeen_typecheck::result::TypeCheckResult,
    resolution: zeen_resolve::ResolutionResult,
    rodeo: Rc<RefCell<Rodeo>>,
}

fn compile(src: &str) -> Result<Compiled, Vec<String>> {
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
        CompilationMode::Debug,
    )
    .unwrap();

    Ok(Compiled {
        program: lowered.program,
        typecheck: typecheck_result,
        resolution: resolution_result,
        rodeo,
    })
}

fn run(src: &str) -> Result<FlowResult, Vec<String>> {
    let mut compiled = compile(src)?;

    run_dataflow(
        &mut compiled.program,
        &mut compiled.typecheck,
        &compiled.resolution,
        compiled.rodeo,
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

fn flow_err_span(src: &str) -> Vec<FlowError> {
    let mut compiled = compile(src).expect("compilation must succeed");
    match run_dataflow(
        &mut compiled.program,
        &mut compiled.typecheck,
        &compiled.resolution,
        compiled.rodeo,
    ) {
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

// --> Closure consumption (S6: env-kind in the fat type)

#[test]
fn fnonce_double_call_is_error() {
    let errors = flow_err(
        r#"
struct Wrap { pub v: i32 }
fn main() i32 {
    let w = Wrap { .v = 3 };
    let c = fn(a: i32) i32 { return a + w.v; };
    let r = c(1);
    let s = c(2);
    return r + s;
}
"#,
    );
    assert!(
        !errors.is_empty(),
        "a second `FnOnce` call must be rejected: the value was consumed"
    );
}

#[test]
fn fnonce_use_after_call_is_error() {
    let errors = flow_err(
        r#"
struct Wrap { pub v: i32 }
fn main() i32 {
    let w = Wrap { .v = 3 };
    let c = fn(a: i32) i32 { return a + w.v; };
    let r = c(1);
    let d = c;
    return r;
}
"#,
    );
    assert!(
        !errors.is_empty(),
        "moving an `FnOnce` closure after it was called must be rejected"
    );
}

#[test]
fn fnonce_single_call_passes() {
    flow_ok(
        r#"
struct Wrap { pub v: i32 }
fn main() i32 {
    let w = Wrap { .v = 3 };
    let c = fn(a: i32) i32 { return a + w.v; };
    let r = c(1);
    return r;
}
"#,
    );
}

#[test]
fn fn_once_closure_stays_callable() {
    // `Fn` closures with Copy captures stay copyable: the value is read as
    // copies at the call site and must survive for a second call.
    flow_ok(
        r#"
fn main() i32 {
    let n = 5;
    let add = fn(a: i32) i32 { return a + n; };
    let r = add(1);
    let s = add(2);
    return r + s;
}
"#,
    );
}

#[test]
fn fnonce_closure_passed_as_argument_passes() {
    flow_ok(
        r#"
struct Wrap { pub v: i32 }
fn apply_once(f: FnOnce(i32) i32, x: i32) i32 { return f(x); }
fn main() i32 {
    let w = Wrap { .v = 3 };
    let c = fn(a: i32) i32 { return a + w.v; };
    return apply_once(c, 10);
}
"#,
    );
}

#[test]
fn fnonce_double_pass_is_error() {
    let errors = flow_err(
        r#"
struct Wrap { pub v: i32 }
fn apply_once(f: FnOnce(i32) i32, x: i32) i32 { return f(x); }
fn main() i32 {
    let w = Wrap { .v = 3 };
    let c = fn(a: i32) i32 { return a + w.v; };
    let r = apply_once(c, 10);
    let s = apply_once(c, 20);
    return r + s;
}
"#,
    );
    assert!(
        !errors.is_empty(),
        "passing the same `FnOnce` closure twice must be rejected"
    );
}

#[test]
fn live_uncalled_fnonce_closure_passes_scope_exit() {
    // The closure is borrowed (not moved), so it stays live until the scope
    // ends, where flow inserts its drop (the heap env is freed by codegen).
    flow_ok(
        r#"
struct Wrap { pub v: i32 }
fn main() i32 {
    let w = Wrap { .v = 3 };
    let c = fn(a: i32) i32 { return a + w.v; };
    let p = &c;
    return 0;
}
"#,
    );
}

#[test]
fn fnonce_param_called_twice_is_error() {
    let errors = flow_err(
        r#"
fn apply_twice(f: FnOnce(i32) i32, x: i32) i32 {
    let r = f(x);
    let s = f(x);
    return r + s;
}
fn main() i32 {
    return apply_twice(fn(a: i32) i32 { return a + 1; }, 41);
}
"#,
    );
    assert!(
        !errors.is_empty(),
        "calling an `FnOnce` param twice inside the callee must be rejected"
    );
}

#[test]
fn fat_value_move_transfers_ownership() {
    // `FnOnce` closure values are move-only: binding one to a new name
    // moves it, so the old name is gone.
    let errors = flow_err(
        r#"
struct Wrap { pub v: i32 }
fn main() i32 {
    let w = Wrap { .v = 3 };
    let c = fn(a: i32) i32 { return a + w.v; };
    let g = c;
    return c(1) + g(2);
}
"#,
    );
    assert!(
        !errors.is_empty(),
        "using an `FnOnce` closure after it was moved must be rejected"
    );
}

#[test]
fn fn_closure_value_is_copy() {
    // `Fn` closure values hold only Copy captures, so the value itself is
    // Copy: copying it to a new name keeps both usable.
    flow_ok(
        r#"
fn main() i32 {
    let n = 5;
    let add = fn(a: i32) i32 { return a + n; };
    let g = add;
    return add(1) + g(2);
}
"#,
    );
}

#[test]
fn fat_value_move_then_single_owner_passes() {
    flow_ok(
        r#"
fn main() i32 {
    let n = 5;
    let add = fn(a: i32) i32 { return a + n; };
    let g = add;
    return g(1) + g(2);
}
"#,
    );
}

#[test]
fn captured_closure_env_cascade_passes() {
    // A closure capturing another closure: the inner fat value moves into
    // the outer env, and the outer's drop must tear the whole chain down.
    flow_ok(
        r#"
fn main() i32 {
    let base = 100;
    let inner = fn(x: i32) i32 { return x + base; };
    let outer = fn(x: i32) i32 { return inner(x) * 2; };
    let r = outer(5);
    return r;
}
"#,
    );
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

#[test]
fn dropped_local_registers_its_function() {
    let result = flow_ok(
        r#"
struct Buffer { pub x: i32 }
implement Drop : Buffer {
    fn drop(self) void {}
}
fn main() {
    let b = Buffer { .x = 1 };
}
"#,
    );
    assert!(!result.functions_with_drops.is_empty());
}

#[test]
fn copy_struct_multiple_uses_passes() {
    flow_ok(
        r#"
struct S {}
implement Copy : S {}
fn main() {
    let a = S {};
    let b = a;
    let c = a;
}
"#,
    );
}

#[test]
fn used_locals_produce_no_warnings() {
    let result = flow_ok(
        r#"
struct Pair { pub a: i32, pub b: i32 }
fn destroy(p: Pair) {}
fn main() {
    let p = Pair { .a = 1, .b = 2 };
    destroy(p);
}
"#,
    );
    assert!(result.warnings.is_empty());
}

#[test]
fn unused_variable_produces_warning() {
    let result = flow_ok("fn main() { let x = 1; }");
    assert!(
        result
            .warnings
            .iter()
            .any(|e| matches!(e, FlowError::UnusedVariable { .. })),
        "expected an UnusedVariable warning, got: {:?}",
        result.warnings
    );
}

#[test]
fn variable_used_as_slice_index_is_not_unused() {
    let result = flow_ok(
        r#"
fn last_element[T](slice: []T) T {
    let last_index = slice.len - 1;
    slice[last_index]
}

fn main() {
    let arr = [1, 2, 3, 4];
    let slice = &arr;
    let _elem = last_element(slice);
}
"#,
    );
    assert!(
        result.warnings.is_empty(),
        "expected no warnings, got: {:?}",
        result.warnings
    );
}

#[test]
fn pointer_type_is_copy() {
    flow_ok(
        r#"
fn main() {
    let x = 5;
    let p = &x;
    let a = p;
    let b = p;
}
"#,
    );
}

#[test]
fn pointer_passed_to_function_multiple_times() {
    flow_ok(
        r#"
fn peek(p: *i32) {}
fn main() {
    let x = 5;
    let p = &x;
    peek(p);
    peek(p);
}
"#,
    );
}

#[test]
fn array_of_copy_elements_is_copyable() {
    flow_ok(
        r#"
fn main() {
    let a = [1, 2, 3];
    let b = a;
    let c = a;
}
"#,
    );
}

#[test]
fn string_literal_array_is_copyable() {
    flow_ok(
        r#"
fn main() {
    let s = "hello";
    let t = s;
    let u = s;
}
"#,
    );
}

#[test]
fn array_of_copy_elements_passed_to_function_multiple_times() {
    flow_ok(
        r#"
fn max(a: [3]i32) i32 {
    return a[0];
}
fn main() {
    let arr = [1, 2, 3];
    max(arr);
    max(arr);
}
"#,
    );
}

#[test]
fn array_element_reads_do_not_move_the_array() {
    flow_ok(
        r#"
fn main() {
    let a = [1, 2, 3];
    let x = a[0];
    let y = a[1];
}
"#,
    );
}

#[test]
fn array_of_move_only_elements_is_still_move_only() {
    let errors = flow_err(
        r#"
struct Buffer { pub x: i32 }
fn main() {
    let a = [Buffer { .x = 1 }, Buffer { .x = 2 }];
    let b = a;
    let c = a;
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseAfterMove")));
}

#[test]
fn array_of_drop_elements_is_still_move_only() {
    let errors = flow_err(
        r#"
struct Buffer { pub x: i32 }
implement Drop : Buffer {
    fn drop(self) void {}
}
fn main() {
    let a = [Buffer { .x = 1 }, Buffer { .x = 2 }];
    let b = a;
    let c = a;
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseAfterMove")));
}

#[test]
fn slice_is_copy_and_usable_multiple_times() {
    flow_ok(
        r#"
fn main() {
    let arr: [3]i32 = [1, 2, 3];
    let s: []i32 = &arr;
    let t = s;
    let u = s;
}
"#,
    );
}

#[test]
fn slice_passed_to_function_multiple_times_is_ok() {
    flow_ok(
        r#"
fn sum(s: []i32) i32 {
    return s[0];
}
fn main() {
    let arr: [3]i32 = [1, 2, 3];
    let s: []i32 = &arr;
    sum(s);
    sum(s);
}
"#,
    );
}

#[test]
fn slice_element_reads_do_not_copy_the_slice() {
    flow_ok(
        r#"
fn main() {
    let arr: [3]i32 = [1, 2, 3];
    let s: []i32 = &arr;
    let a = s[0];
    let b = s[1];
}
"#,
    );
}

#[test]
fn addr_of_array_yields_a_slice() {
    flow_ok(
        r#"
fn main() {
    let a: [4]i32 = [1, 2, 3, 4];
    let b: []i32 = &a;
    let _ = b[2];
}
"#,
    );
}

#[test]
fn addr_of_array_never_yields_pointer_to_array() {
    // `&array` types as a slice, never as `*[N]T`; a supposed pointer-to-array
    // annotation must fail to compile with a type mismatch.
    assert!(
        compile(
            r#"
fn main() {
    let a: [4]i32 = [1, 2, 3, 4];
    let b: *[4]i32 = &a;
}
"#,
        )
        .is_err(),
        "`&array` must never type as `*[N]T`, only as a slice"
    );
}

#[test]
fn generic_struct_is_move_only_even_with_copy_field() {
    let errors = flow_err(
        r#"
struct Box[T] { pub v: T }
fn main() {
    let b = Box { .v = 30 };
    let c = b;
    let d = b;
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseAfterMove")));
}

#[test]
fn generic_struct_partial_move_is_error_on_whole_use() {
    let errors = flow_err(
        r#"
struct Inner { pub x: i32 }
struct Box[T] { pub v: T }
fn main() {
    let b = Box { .v = Inner { .x = 1 } };
    let v = b.v;
    let u = b;
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseOfPartiallyMoved")));
}

#[test]
fn generic_struct_field_can_be_reinitialized_and_moved_whole() {
    flow_ok(
        r#"
struct Inner { pub x: i32 }
struct Box[T] { pub v: T }
fn main() {
    let b = Box { .v = Inner { .x = 1 } };
    let v = b.v;
    b.v = Inner { .x = 2 };
    let whole = b;
}
"#,
    );
}

#[test]
fn generic_struct_field_read_does_not_move_copy_concrete_type() {
    flow_ok(
        r#"
struct Box[T] { pub v: T }
fn main() {
    let b = Box { .v = 30 };
    let x = b.v;
    let y = b.v;
}
"#,
    );
}

#[test]
fn generic_struct_field_move_is_detected_for_move_only_concrete_type() {
    let errors = flow_err(
        r#"
struct Inner { pub x: i32 }
struct Box[T] { pub v: T }
fn main() {
    let b = Box { .v = Inner { .x = 1 } };
    let x = b.v;
    let y = b.v;
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseAfterMove")));
}

#[test]
fn uninitialized_struct_used_whole_before_all_fields_written_is_error() {
    let errors = flow_err(
        r#"
struct Inner { pub x: i32 }
struct Pair { pub a: Inner, pub b: Inner }
fn main() {
    let p: Pair;
    p.a = Inner { .x = 1 };
    let q = p;
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseOfPartiallyMoved")));
}

#[test]
fn uninitialized_struct_fully_reconstructed_fieldwise_is_usable() {
    flow_ok(
        r#"
struct Inner { pub x: i32 }
struct Pair { pub a: Inner, pub b: Inner }
fn main() {
    let p: Pair;
    p.a = Inner { .x = 1 };
    p.b = Inner { .x = 2 };
    let q = p;
}
"#,
    );
}

#[test]
fn moved_struct_field_rebuilt_only_partially_is_still_error() {
    let errors = flow_err(
        r#"
struct Inner { pub x: i32 }
struct Pair { pub a: Inner, pub b: Inner }
fn main() {
    let p = Pair { .a = Inner { .x = 1 }, .b = Inner { .x = 2 } };
    let _ = p.a;
    p.b = Inner { .x = 3 };
    let q = p;
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseOfPartiallyMoved")));
}

#[test]
fn whole_local_reassigned_after_move_is_usable() {
    flow_ok(
        r#"
struct Inner { pub x: i32 }
fn main() {
    let p = Inner { .x = 1 };
    let q = p;
    p = Inner { .x = 2 };
    let r = p;
}
"#,
    );
}

#[test]
fn generic_function_param_move_is_detected() {
    let errors = flow_err(
        r#"
struct Inner { pub x: i32 }
fn id[T](v: T) T {
    return v;
}
fn main() {
    let i = Inner { .x = 1 };
    let a = id(i);
    let b = id(i);
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseAfterMove")));
}

#[test]
fn generic_function_copy_param_passes() {
    flow_ok(
        r#"
fn id[T](v: T) T {
    return v;
}
fn main() {
    let a = id(30);
    let b = id(40);
}
"#,
    );
}

#[test]
fn ref_receiver_method_does_not_move_then_ownership_method_consumes() {
    flow_ok(
        r#"
struct Foo {
    pub fn new() Self {
        Self {}
    }

    pub fn no_ownership(*self) {}
    pub fn ownership(self) {}
}
implement Drop : Foo {
    fn drop(self) {
        
    }
}
fn main() {
    let a = Foo.new();
    let b = a;

    b.no_ownership();
    b.ownership();
}
"#,
    );
}

#[test]
fn enum_variant_value_is_copy() {
    flow_ok(
        r#"
enum Color { Red, Green, Blue }
fn main() {
    let c = Color.Red;
    let d = c;
    let e = c;
}
"#,
    );
}

#[test]
fn generic_drop_struct_field_move_is_error() {
    let errors = flow_err(
        r#"
struct Inner { pub x: i32 }
struct Box[T] { pub v: T }
implement[T] Drop : Box[T] {
    fn drop(self) void {}
}
fn main() {
    let b = Box { .v = Inner { .x = 1 } };
    let v = b.v;
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("MoveOutOfDrop")));
}

#[test]
fn generic_drop_struct_registers_scope_drop() {
    let result = flow_ok(
        r#"
struct Box[T] { pub v: T }
implement[T] Drop : Box[T] {
    fn drop(self) void {}
}
fn main() {
    let b = Box { .v = 30 };
}
"#,
    );
    assert!(!result.functions_with_drops.is_empty());
}

#[test]
fn many_pointer_indexing_via_cast_works() {
    flow_ok(
        r#"
fn main() {
    let x = 5;
    let p: [*]i32 = @as([*]i32, &x);
    let a = p[0];
    let b = p[1];
}
"#,
    );
}

#[test]
fn underscore_let_binds_no_mir_local() {
    let compiled = compile(
        r#"
fn main() {
    let a = 5;
    let _ = a;
    let _ = 11 + 7;
}
"#,
    )
    .unwrap();

    let main_id = compiled
        .program
        .function_names
        .iter()
        .find(|(_, name)| name.as_str() == "main")
        .map(|(id, _)| *id)
        .unwrap();
    let main_fn = compiled.program.functions.get(&main_id).unwrap();

    let names: Vec<String> = main_fn
        .locals
        .iter()
        .filter_map(|local| {
            local
                .name
                .map(|spur| compiled.rodeo.borrow().resolve(&spur).to_string())
        })
        .collect();
    assert!(
        !names.iter().any(|n| n == "_"),
        "`let _ =` must not allocate a `_` local, got: {names:?}"
    );
}

#[test]
fn underscore_let_suppresses_unused_variable() {
    let result = flow_ok(
        r#"
fn main() {
    let a = 5;
    let _ = a;
    let _ = 11 + 7;
}
"#,
    );
    assert!(
        result.warnings.is_empty(),
        "expected no warnings, got: {:?}",
        result.warnings
    );
}

#[test]
fn underscore_let_still_consumes_moved_values() {
    let errors = flow_err(
        r#"
struct Inner { pub x: i32 }
fn main() {
    let p = Inner { .x = 1 };
    let _ = p;
    let q = p;
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseAfterMove")));
}

#[test]
fn underscore_let_of_uninitialized_value_is_error() {
    let errors = flow_err(
        r#"
struct Inner { pub x: i32 }
fn main() {
    let x: Inner;
    let _ = x;
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseOfUninitialized")));
}

#[test]
fn copy_and_drop_implementations_are_an_error() {
    let errors = flow_err(
        r#"
struct Foo { pub x: i32 }
implement Copy : Foo {}
implement Drop : Foo {
    fn drop(self) void {}
}
fn main() {}
"#,
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("CopyWithDrop") || e.contains("Copy`")),
        "expected the dataflow to reject Copy + Drop, got: {errors:?}"
    );
}

#[test]
fn copy_only_or_drop_only_are_allowed() {
    flow_ok(
        r#"
struct CopyFoo { pub x: i32 }
implement Copy : CopyFoo {}
fn main() {
    let a = CopyFoo { .x = 1 };
    let b = a;
    let c = a;
}
"#,
    );
    flow_ok(
        r#"
struct DropFoo { pub x: i32 }
implement Drop : DropFoo {
    fn drop(self) void {}
}
fn main() {
    let a = DropFoo { .x = 1 };
}
"#,
    );
}

#[test]
fn drop_impls_are_registered_without_recursing_on_self() {
    let mut compiled = compile(
        r#"
struct Foo {
}
implement Drop : Foo {
    fn drop(self) void {
    }
}
fn take_dropper[T: Drop](x: T) void {
}
fn main() {
    take_dropper(Foo {});
}
"#,
    )
    .unwrap();

    run_dataflow(
        &mut compiled.program,
        &mut compiled.typecheck,
        &compiled.resolution,
        Rc::clone(&compiled.rodeo),
    )
    .expect("dataflow must pass");

    let drop_id = compiled
        .program
        .function_names
        .iter()
        .find(|(_, name)| name.as_str() == "Foo.drop")
        .map(|(id, _)| *id)
        .expect("`Foo.drop` must be registered");
    let self_local = compiled.program.functions[&drop_id].params[0];

    let has_self_drop = compiled.program.functions[&drop_id]
        .blocks
        .iter()
        .any(|block| {
            block.statements.iter().any(
            |stmt| matches!(stmt, zeen_mir::MirStatement::Drop(place) if place.local == self_local),
        )
        });
    assert!(
        !has_self_drop,
        "the drop impl must not auto-drop its own `self` parameter"
    );
}

/// Formats the projection of a `Drop` statement target as a display string
/// (e.g. `%0` for a whole local, `%2.field1.field2` for a nested field).
fn drop_targets(src: &str) -> Vec<String> {
    let mut compiled = compile(src).expect("compilation must succeed");
    run_dataflow(
        &mut compiled.program,
        &mut compiled.typecheck,
        &compiled.resolution,
        Rc::clone(&compiled.rodeo),
    )
    .expect("dataflow must pass");

    let main_id = compiled
        .program
        .function_names
        .iter()
        .find(|(_, name)| name.as_str() == "main")
        .map(|(id, _)| *id)
        .expect("`main` must exist");
    let main_fn = &compiled.program.functions[&main_id];

    let mut targets = Vec::new();
    for block in &main_fn.blocks {
        for stmt in &block.statements {
            let zeen_mir::MirStatement::Drop(place) = stmt else {
                continue;
            };
            let mut target = format!("%{}", place.local.0);
            for elem in &place.projection {
                if let zeen_mir::PlaceElem::Field(field) = elem {
                    target.push_str(&format!(".{}", field.0));
                }
            }
            targets.push(target);
        }
    }
    targets
}

#[test]
fn non_drop_struct_drops_its_drop_fields_recursively() {
    let targets = drop_targets(
        r#"
struct Leaf { pub x: i32 }
implement Drop : Leaf { fn drop(self) void {} }
struct Wrapper { pub a: Leaf, pub b: Leaf }
fn main() {
    let w = Wrapper { .a = Leaf { .x = 1 }, .b = Leaf { .x = 2 } };
}
"#,
    );
    assert!(
        !targets
            .iter()
            .any(|t| t.starts_with("%0") && !t.contains('.')),
        "a non-Drop struct must not be dropped whole, got: {targets:?}"
    );
    assert_eq!(targets.len(), 2, "got: {targets:?}");
}

#[test]
fn explicit_drop_struct_is_dropped_whole() {
    let targets = drop_targets(
        r#"
struct Leaf { pub x: i32 }
implement Drop : Leaf { fn drop(self) void {} }
fn main() {
    let l = Leaf { .x = 1 };
}
"#,
    );
    assert!(
        targets.iter().any(|t| t == "%0"),
        "an explicit-Drop struct must be dropped as a whole value, got: {targets:?}"
    );
}

#[test]
fn partially_moved_struct_drops_only_live_fields_deep() {
    let targets = drop_targets(
        r#"
struct Leaf { pub x: i32 }
implement Drop : Leaf { fn drop(self) void {} }
struct Inner { pub a: Leaf, pub b: Leaf }
struct Outer { pub i: Inner, pub j: Leaf }
fn main() {
    let o = Outer { .i = Inner { .a = Leaf { .x = 1 }, .b = Leaf { .x = 2 } }, .j = Leaf { .x = 3 } };
    let _ = o.j;
}
"#,
    );
    assert!(
        !targets
            .iter()
            .any(|t| t.starts_with("%0") && !t.contains('.')),
        "must not drop the whole `o`, got: {targets:?}"
    );
    assert!(
        !targets.iter().any(|t| t == "%0.i"),
        "must not drop the whole live `Inner`, got: {targets:?}"
    );
    let prefixes: std::collections::HashSet<String> = targets
        .iter()
        .map(|t| {
            let mut parts = t.splitn(3, '.');
            let root = parts.next().unwrap();
            let inner = parts.next().unwrap_or("").to_string();
            let leaf = parts.next().unwrap_or("").to_string();
            (root.to_string(), inner, leaf)
        })
        .filter(|(_, _, leaf)| !leaf.is_empty())
        .map(|(_, inner, _)| inner)
        .collect();
    assert_eq!(
        prefixes.len(),
        1,
        "expected both live leaves under `o.i`, got: {targets:?}"
    );
    assert_eq!(targets.len(), 2, "got: {targets:?}");
}

#[test]
fn nested_block_scope_drops_its_locals_at_block_end() {
    /// `(%local, Drop)` pairs in statement order.
    fn drops_in(src: &str) -> Vec<(usize, String)> {
        let mut compiled = compile(src).expect("compilation must succeed");
        run_dataflow(
            &mut compiled.program,
            &mut compiled.typecheck,
            &compiled.resolution,
            Rc::clone(&compiled.rodeo),
        )
        .expect("dataflow must pass");

        let main_id = compiled
            .program
            .function_names
            .iter()
            .find(|(_, name)| name.as_str() == "main")
            .map(|(id, _)| *id)
            .unwrap();
        let main_fn = &compiled.program.functions[&main_id];

        let mut drops = Vec::new();
        for block in &main_fn.blocks {
            for stmt in &block.statements {
                let zeen_mir::MirStatement::Drop(place) = stmt else {
                    continue;
                };
                let mut label = format!("%{}", place.local.0);
                for elem in &place.projection {
                    if let zeen_mir::PlaceElem::Field(field) = elem {
                        label.push_str(&format!(".{}", field.0));
                    }
                }
                drops.push((place.local.0 as usize, label));
            }
        }
        drops
    }

    // A drop-typed local inside a nested block is dropped at the end of that
    // block (its own `StorageDead`) and not only at function exit. The green
    // local is dropped twice: once for its block scope, once... no - a local
    // must be dropped exactly once. This function has no explicit drops, so the
    // scope-end drop is the only drop of the block-scoped value.
    let drops = drops_in(
        r#"
struct Buffer { pub x: i32 }
implement Drop : Buffer {
    fn drop(self) void {}
}
fn main() {
    let keep = Buffer { .x = 1 };
    if (1 == 1) {
        let inner = Buffer { .x = 2 };
    };
}
"#,
    );
    // `%0` (keep) is dropped at function exit; the block-scoped `inner` is
    // dropped at its block's `StorageDead` (before the function-exit drop).
    // Both appear exactly once.
    assert_eq!(drops, vec![(3, "%3".into()), (0, "%0".into())]);
}

#[test]
fn maybe_uninitialized_at_scope_end_is_an_error() {
    let errors = flow_err(
        r#"
struct Buffer { pub x: i32 }
implement Drop : Buffer {
    fn drop(self) void {}
}
fn main() {
    let go = 1;
    if (go == 1) {
        let guard: Buffer;
        let inner_go = 1;
        if (inner_go == 1) {
            guard = Buffer { .x = 1 };
        };
    };
}
"#,
    );
    assert!(
        errors.iter().any(|e| e.contains("MaybeUninitializedDrop")),
        "expected a maybe-uninitialized drop error, got: {errors:?}"
    );
}

#[test]
fn block_trailing_value_moved_out_of_scope_passes() {
    flow_ok(
        r#"
struct Buffer { pub x: i32 }
implement Drop : Buffer {
    fn drop(self) void {}
}
fn main() {
    let _a = Buffer { .x = 1 };

    let _b = {
        let temporary = Buffer { .x = 2 };
        let ownered = temporary;

        ownered
    };
}
"#,
    );
}

#[test]
fn block_trailing_value_copy_passes() {
    flow_ok(
        r#"
fn main() {
    let _b = {
        let temporary = 1;
        let ownered = temporary;
        ownered
    };
}
"#,
    );
}

#[test]
fn if_branches_returning_block_scoped_drop_values_passes() {
    flow_ok(
        r#"
struct Buffer { pub x: i32 }
implement Drop : Buffer {
    fn drop(self) void {}
}
fn main() {
    let go = 1;
    let _b = if (go == 1) {
        let temporary = Buffer { .x = 2 };
        temporary
    } else {
        let other = Buffer { .x = 3 };
        other
    };
}
"#,
    );
}

#[test]
fn block_scoped_value_is_dropped_when_not_consumed() {
    let targets = drop_targets(
        r#"
struct Buffer { pub x: i32 }
implement Drop : Buffer {
    fn drop(self) void {}
}
fn main() {
    let _ignore = {
        let temporary = Buffer { .x = 1 };
        if (1 == 1) {
            let inner = Buffer { .x = 2 };
        };
        temporary
    };
}
"#,
    );
    assert!(
        targets.iter().any(|t| t.starts_with("%")),
        "expected drop targets, got: {targets:?}"
    );
}

#[test]
fn use_after_move_points_at_the_operand_not_the_statement() {
    let src = "struct Pair { pub x: i32 }\nfn main() {\n    let a = Pair { .x = 1 };\n    let b = a;\n    let c = a;\n}\n";
    let errors = flow_err_span(src);
    assert_eq!(errors.len(), 1, "got: {errors:?}");

    let FlowError::UseAfterMove { span, .. } = &errors[0] else {
        panic!("expected UseAfterMove, got: {errors:?}");
    };

    let operand_offset = src.find("let c = ").expect("statement present") + "let c = ".len();
    assert_eq!(
        span.offset(),
        operand_offset,
        "error must label `a`, not the whole statement"
    );
    assert_eq!(
        span.len(),
        1,
        "error must label only the single `a` identifier"
    );
}

/// Labels every statement of `main`, in statement order, so test can assert
/// relative positions of `Drop`s vs `StorageLive`/`StorageDead`.
fn main_stmt_labels(src: &str) -> Vec<String> {
    let mut compiled = compile(src).expect("compilation must succeed");
    run_dataflow(
        &mut compiled.program,
        &mut compiled.typecheck,
        &compiled.resolution,
        Rc::clone(&compiled.rodeo),
    )
    .expect("dataflow must pass");

    let main_id = compiled
        .program
        .function_names
        .iter()
        .find(|(_, name)| name.as_str() == "main")
        .map(|(id, _)| *id)
        .expect("`main` must exist");
    let main_fn = &compiled.program.functions[&main_id];

    let mut labels = Vec::new();
    for block in &main_fn.blocks {
        for stmt in &block.statements {
            match stmt {
                zeen_mir::MirStatement::StorageLive(local) => {
                    labels.push(format!("StorageLive(%{})", local.0));
                }
                zeen_mir::MirStatement::StorageDead(local) => {
                    labels.push(format!("StorageDead(%{})", local.0));
                }
                zeen_mir::MirStatement::Drop(place) => {
                    labels.push(format!("Drop(%{})", place.local.0));
                }
                zeen_mir::MirStatement::Discard(_) => labels.push("Discard".into()),
                zeen_mir::MirStatement::Assign { place, .. } => {
                    labels.push(format!("Assign(%{})", place.local.0));
                }
                zeen_mir::MirStatement::Nop => labels.push("Nop".into()),
            }
        }
    }
    labels
}

#[test]
fn scope_drop_hoists_to_the_last_use_of_the_local() {
    // `only` is read (`let _ = only.x`) before a later block-scoped local is
    // created. The whole-value drop must be emitted right after that last read,
    // not at the end of the block scope.
    let labels = main_stmt_labels(
        r#"
struct Buffer { pub x: i32 }
implement Drop : Buffer {
    fn drop(self) void {}
}
fn main() {
    if (1 == 1) {
        let only = Buffer { .x = 1 };
        let _ = only.x;
        let later = Buffer { .x = 2 };
    };
}
"#,
    );

    let (drops, lives) = drop_and_live_ids(&labels);
    assert_eq!(
        drops.len(),
        2,
        "two block-scoped drops expected: {labels:?}"
    );

    // `only`'s drop must be hoisted to right after its read, i.e. before the
    // other local's `StorageLive` (its `let later = ...` initializer).
    let (drop_only, drop_later) = (drops[0], drops[1]);
    let drop_only_pos = labels
        .iter()
        .position(|l| l == &format!("Drop(%{drop_only})"))
        .expect("drop of `only` must exist");
    let drop_later_pos = labels
        .iter()
        .position(|l| l == &format!("Drop(%{drop_later})"))
        .expect("drop of `later` must exist");
    let live_later_pos = lives
        .iter()
        .find(|id| **id != drop_only)
        .and_then(|id| {
            labels
                .iter()
                .position(|l| l == &format!("StorageLive(%{id})"))
        })
        .expect("StorageLive of `later` must exist");

    assert!(
        drop_only_pos < live_later_pos,
        "the drop of `only` must be hoisted before `later` is created\nsequence: {labels:?}"
    );
    // The hoisted drop must sit immediately after the read (`let _ = only.x;`).
    assert!(
        labels
            .windows(2)
            .any(|w| w[0] == "Discard" && w[1] == format!("Drop(%{drop_only})")),
        "the hoisted drop must follow the read of `only`\nsequence: {labels:?}"
    );
    assert!(
        drop_later_pos > drop_only_pos,
        "`later`'s drop (at its scope end) must come after the hoisted `only` drop\n\
         sequence: {labels:?}"
    );
}

#[test]
fn scope_drop_stays_at_scope_end_when_local_is_rewritten() {
    // `only` is read mid-block but then reassigned. Hoisting the drop past the
    // reassignment would drop the new value at the wrong place, so the drop
    // must remain right before the scope-end `StorageDead`.
    let labels = main_stmt_labels(
        r#"
struct Buffer { pub x: i32 }
implement Drop : Buffer {
    fn drop(self) void {}
}
fn main() {
    if (1 == 1) {
        let only = Buffer { .x = 1 };
        let _ = only.x;
        only = Buffer { .x = 2 };
    };
}
"#,
    );

    let (drops, _) = drop_and_live_ids(&labels);
    assert_eq!(drops.len(), 1, "exactly one drop expected: {labels:?}");
    let dead_only = labels
        .iter()
        .position(|l| l == &format!("StorageDead(%{})", drops[0]))
        .expect("StorageDead of `only` must exist");
    let drop_pos = labels
        .iter()
        .position(|l| l == &format!("Drop(%{})", drops[0]))
        .expect("drop of `only` must exist");

    // The reassigned value must still be dropped at scope end: the drop sits
    // immediately before the local's `StorageDead`.
    assert_eq!(drop_pos + 1, dead_only, "sequence: {labels:?}");
}

/// `(drop local ids, StorageLive local ids)` in label order.
fn drop_and_live_ids(labels: &[String]) -> (Vec<usize>, Vec<usize>) {
    let drops: Vec<usize> = labels
        .iter()
        .filter_map(|l| {
            l.strip_prefix("Drop(%").map(|r| {
                r.trim_end_matches(')')
                    .parse::<usize>()
                    .expect("local id parse")
            })
        })
        .collect();
    let lives: Vec<usize> = labels
        .iter()
        .filter_map(|l| {
            l.strip_prefix("StorageLive(%").map(|r| {
                r.trim_end_matches(')')
                    .parse::<usize>()
                    .expect("local id parse")
            })
        })
        .collect();
    (drops, lives)
}
