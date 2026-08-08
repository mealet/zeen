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
        &compiled.typecheck,
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
fn array_use_after_move_is_error() {
    let errors = flow_err(
        r#"
fn main() {
    let a = [1, 2, 3];
    let b = a;
    let c = a;
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseAfterMove")));
}

#[test]
fn string_literal_array_use_after_move_is_error() {
    let errors = flow_err(
        r#"
fn main() {
    let s = "hello";
    let t = s;
    let u = s;
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseAfterMove")));
}

#[test]
fn array_moved_into_function_then_used_again_is_error() {
    let errors = flow_err(
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
    assert!(errors.iter().any(|e| e.contains("UseAfterMove")));
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
fn slice_use_after_move_is_error() {
    let errors = flow_err(
        r#"
fn main() {
    let s: []i32 = [1, 2, 3];
    let t = s;
    let u = s;
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseAfterMove")));
}

#[test]
fn slice_passed_to_function_multiple_times_is_error() {
    let errors = flow_err(
        r#"
fn sum(s: []i32) i32 {
    return s[0];
}
fn main() {
    let s: []i32 = [1, 2, 3];
    sum(s);
    sum(s);
}
"#,
    );
    assert!(errors.iter().any(|e| e.contains("UseAfterMove")));
}

#[test]
fn slice_element_reads_do_not_copy_the_slice() {
    flow_ok(
        r#"
fn main() {
    let s: []i32 = [1, 2, 3];
    let a = s[0];
    let b = s[1];
}
"#,
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
        &compiled.typecheck,
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
