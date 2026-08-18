use std::{cell::RefCell, collections::HashSet, path::Path, rc::Rc, sync::Arc};

use bumpalo::Bump;
use lasso::Rodeo;
use zeen_driver::{CompilationContext, CompilationMode, CompilationOutput, PathsConfig};
use zeen_parser::Parser;

use crate::lowering::{MirLoweringResult, lower_program};

const CORE_OPS: &str = include_str!("../../../lib/core/ops.zn");

fn compile_mir_mode(
    src: &str,
    mode: CompilationMode,
) -> Result<(MirLoweringResult, Rc<RefCell<Rodeo>>), Vec<String>> {
    let rodeo = Rc::new(RefCell::new(Rodeo::default()));
    let bump = Bump::default();
    let content = Arc::new(src.to_string());
    let filename = Rc::new("test.zn".to_string());

    let mut context = CompilationContext {
        paths: PathsConfig {
            project_root: std::path::PathBuf::from("/"),
            std_root: None,
            linked: HashSet::new(),
        },
        core_files: vec![("core.ops", CORE_OPS)],
        mode,
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

    let program = parser
        .parse_program()
        .map_err(|errs| errs.iter().map(|e| e.to_string()).collect::<Vec<_>>())?;

    let (resolved_program, mut resolution_result) = zeen_resolve::resolve(
        Rc::clone(&filename),
        Arc::clone(&content),
        Path::new("/test.zn"),
        program,
        &bump,
        Rc::clone(&rodeo),
        &mut context,
    )
    .map_err(|errs| errs.iter().map(|e| e.to_string()).collect::<Vec<_>>())?;

    let mut hir_lowering = zeen_hir::HirLowering::new(&resolution_result, Rc::clone(&rodeo));
    let hir_module = hir_lowering.lower_module(resolved_program);

    let mut typechecker =
        zeen_typecheck::TypeChecker::new(&mut resolution_result, &context, Rc::clone(&rodeo));
    typechecker.check_module(&hir_module);

    let mut typecheck = typechecker
        .finish()
        .map_err(|errs| errs.iter().map(|e| e.to_string()).collect::<Vec<_>>())?;

    let lowered_mir = lower_program(
        Rc::clone(&rodeo),
        &mut typecheck,
        &resolution_result,
        &hir_module,
        mode,
    );

    Ok((lowered_mir, Rc::clone(&rodeo)))
}

fn compile_mir(src: &str) -> Result<(MirLoweringResult, Rc<RefCell<Rodeo>>), Vec<String>> {
    compile_mir_mode(src, CompilationMode::Debug)
}

fn compile_mir_ok(src: &str) -> MirLoweringResult {
    compile_mir(src)
        .unwrap_or_else(|errors| {
            panic!(
                "expected MIR lowering to succeed, got errors:\n{}",
                errors.join("\n")
            )
        })
        .0
}

#[test]
fn implement_operator_fn_is_named_with_struct_owner() {
    let mir = compile_mir_ok(
        "struct Foo {} \
         implement Add : Foo { fn add(self, other: Self) Self { Self {} } } \
         fn main() { let a = Foo {}; let b = Foo {}; let c = a + b; }",
    );

    let names: Vec<String> = mir.program.function_names.values().cloned().collect();

    assert!(
        names.iter().any(|n| n == "Foo.add"),
        "expected `Foo.add` in MIR function names, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "add"),
        "bare `add` name must not be used, got {names:?}"
    );
}

#[test]
fn drop_impl_function_is_registered_with_struct_owner() {
    let mir = compile_mir_ok(
        "struct Foo {} \
         implement Drop : Foo { fn drop(self) void {} } \
         fn take_dropper[T: Drop](x: T) void {} \
         fn main() { take_dropper(Foo {}); }",
    );

    let names: Vec<String> = mir.program.function_names.values().cloned().collect();

    assert!(
        names.iter().any(|n| n == "Foo.drop"),
        "expected a registered `Foo.drop` in MIR function names, got {names:?}"
    );
}

#[test]
fn generic_drop_impl_is_registered_per_concrete_type() {
    let mir = compile_mir_ok(
        "struct Box[T] { pub v: T } \
         implement[T] Drop : Box[T] { fn drop(self) void {} } \
         fn main() { let b = Box { .v = 30 }; }",
    );

    let names: Vec<String> = mir.program.function_names.values().cloned().collect();

    assert!(
        names.iter().any(|n| n == "Box[i32].drop"),
        "expected a registered `Box[i32].drop` in MIR function names, got {names:?}"
    );
}

#[test]
fn address_of_literal_materializes_temp() {
    compile_mir_ok("fn main() { let p: *i32 = &123; let q: i32 = *p; }");
}

#[test]
fn address_of_non_lvalue_expression_materializes_temp() {
    compile_mir_ok("fn main() { let a = 1; let p: *i32 = &(a + 1); let q: i32 = *p; }");
}

#[test]
fn address_of_array_literal_builds_slice() {
    compile_mir_ok("fn main() { let s: []i32 = &[1, 2, 3]; }");
}

#[test]
fn generic_pointer_param_accepts_literal_address() {
    compile_mir_ok(
        "struct Box[T] { pub inner: *T } \
         fn make[T](value: *T) Box[T] { Box { .inner = value } } \
         fn main() { let b = make(&123); }",
    );
}

#[test]
fn auto_deref_field_access_inserts_deref_projection() {
    let mir = compile_mir_ok(
        "struct Foo { pub x: i32 } \
         fn main() { let f = Foo { .x = 1 }; let sf: *Foo = &f; let v: i32 = sf.x; }",
    );

    let has_deref_field = mir.program.functions.values().any(|func| {
        func.blocks.iter().any(|block| {
            block.statements.iter().any(|stmt| {
                if let crate::MirStatement::Assign {
                    rvalue: crate::Rvalue::Use(operand),
                    ..
                } = stmt
                {
                    let place = match operand {
                        crate::Operand::Copy(p, _) | crate::Operand::Move(p, _) => p,
                        crate::Operand::Constant(_, _) => return false,
                    };
                    matches!(
                        place.projection.as_slice(),
                        [crate::PlaceElem::Deref, crate::PlaceElem::Field(_)]
                    )
                } else {
                    false
                }
            })
        })
    });

    assert!(
        has_deref_field,
        "expected a `[Deref, Field]` place for the auto-deref read `sf.x`"
    );
}

#[test]
fn field_access_on_call_result_materializes_temp() {
    compile_mir_ok(
        "struct Foo { pub a: i32 } \
         fn make() Foo { Foo { .a = 1 } } \
         fn main() { let v: i32 = make().a; }",
    );
}

#[test]
fn method_call_on_call_result_materializes_receiver() {
    compile_mir_ok(
        "struct Foo { pub a: i32 } \
         fn make() Foo { Foo { .a = 1 } } \
         fn main() { let v: i32 = make().a; }",
    );
}

#[test]
fn deref_of_call_result_is_lvalue() {
    compile_mir_ok(
        "extern fn malloc(usize) *void; \
         fn make_ptr() *i32 { let p: *i32 = malloc(4); *p = 5; p } \
         fn main() { let v: i32 = *make_ptr(); *make_ptr() = 7; }",
    );
}

#[test]
fn slice_index_on_call_result_materializes_slice() {
    compile_mir_ok(
        "fn get_slice() []i32 { let arr = [1, 2, 3]; return &arr; } \
         fn main() { let v: i32 = get_slice()[1]; }",
    );
}

#[test]
fn for_loop_over_array_literal_materializes_iterator() {
    compile_mir_ok("fn main() { for (element : [123, 321, 333]) { @println(\"{}\", element); } }");
}

#[test]
fn for_loop_over_rvalue_slice_materializes_iterator() {
    compile_mir_ok(
        "fn get_slice() []i32 { let arr = [1, 2, 3]; return &arr; } \
         fn main() { for (element : get_slice()) { @println(\"{}\", element); } }",
    );
}

#[test]
fn function_name_used_as_value_lowers_to_fn_constant() {
    let mir = compile_mir_ok(
        "fn foo() i32 { 123 } \
         fn main() { let f = foo; let r = f(); @println(\"{}\", r); }",
    );

    let has_fn_const = mir.program.functions.values().any(|func| {
        func.blocks.iter().any(|block| {
            block.statements.iter().any(|stmt| {
                matches!(
                    stmt,
                    crate::MirStatement::Assign {
                        rvalue: crate::Rvalue::Use(crate::Operand::Constant(
                            crate::ConstValue::Fn(_),
                            _
                        )),
                        ..
                    }
                )
            })
        })
    });

    assert!(
        has_fn_const,
        "expected a `ConstValue::Fn` assignment for `let f = foo;`"
    );
}

#[test]
fn fn_typed_param_is_called_indirectly() {
    compile_mir_ok(
        "fn apply(f: fn(i32) i32, x: i32) i32 { f(x) } \
         fn inc(x: i32) i32 { x + 1 } \
         fn main() { let r = apply(inc, 1); @println(\"{}\", r); }",
    );
}

#[test]
fn nested_fn_is_registered_with_parent_prefixed_name() {
    let mir = compile_mir_ok("fn main() { fn foo() void { @println(\"hi\"); } foo(); }");

    let names: Vec<String> = mir.program.function_names.values().cloned().collect();

    assert!(
        names.iter().any(|n| n == "main->foo"),
        "expected a registered `main->foo` in MIR function names, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "foo"),
        "bare `foo` name must not be used for a nested function, got {names:?}"
    );
}

#[test]
fn nested_fn_is_lowered_only_when_called() {
    let mir = compile_mir_ok("fn main() { fn unused() void { @println(\"nope\"); } }");

    let names: Vec<String> = mir.program.function_names.values().cloned().collect();

    assert!(
        !names.iter().any(|n| n == "main->unused"),
        "uncalled nested function must not be eagerly lowered, got {names:?}"
    );
}

#[test]
fn deeply_nested_fn_uses_full_parent_chain() {
    let mir = compile_mir_ok(
        "fn main() { \
             fn inner() void { \
                 fn deepest() void { @println(\"deep\"); } \
                 deepest(); \
             } \
             inner(); \
         }",
    );

    let names: Vec<String> = mir.program.function_names.values().cloned().collect();

    assert!(
        names.iter().any(|n| n == "main->inner->deepest"),
        "expected `main->inner->deepest` in MIR function names, got {names:?}"
    );
}

#[test]
fn generic_nested_fn_includes_concrete_args() {
    let mir = compile_mir_ok(
        "fn main() { \
             fn id[T](x: T) T { x } \
             let a = id(123); \
             let b = id(1.5); \
             @println(\"{}\", a); \
             @println(\"{}\", b); \
         }",
    );

    let names: Vec<String> = mir.program.function_names.values().cloned().collect();

    assert!(
        names.iter().any(|n| n == "main->id[i32]"),
        "expected `main->id[i32]` in MIR function names, got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "main->id[f64]"),
        "expected `main->id[f64]` in MIR function names, got {names:?}"
    );
}

#[test]
fn struct_format_arg_is_lowered_to_display_call() {
    let mir = compile_mir_ok(
        "struct Foo {} \
         implement Display : Foo { fn display(*const self) []const char { return \"foo\"; } } \
         fn main() { let f = Foo {}; @println(\"{}\", f); }",
    );

    // The display method must be monomorphized: before the fix the format
    // machinery never invoked it, so no `Foo.display` was emitted.
    let names: Vec<String> = mir.program.function_names.values().cloned().collect();
    assert!(
        names.iter().any(|n| n == "Foo.display"),
        "expected `Foo.display` in MIR function names, got {names:?}"
    );

    let display_id = mir
        .program
        .function_names
        .iter()
        .find(|(_, n)| n.as_str() == "Foo.display")
        .map(|(id, _)| *id)
        .expect("Foo.display id");

    let main_id = mir
        .program
        .function_names
        .iter()
        .find(|(_, n)| n.as_str() == "main")
        .map(|(id, _)| *id)
        .expect("main id");
    let main = &mir.program.functions[&main_id];

    // The format argument is produced by a direct call to `Foo.display`
    // right before the println macro call.
    let calls_display = main.blocks.iter().any(|b| {
        matches!(
            b.terminator,
            crate::Terminator::Call {
                func: crate::CallTarget::Direct(id),
                ..
            } if id == display_id
        )
    });
    assert!(calls_display, "expected a call to `Foo.display` in main");

    // println must receive exactly one argument (the display result).
    let println_has_single_arg = main.blocks.iter().any(|b| {
        matches!(
            b.terminator,
            crate::Terminator::MacroCall {
                kind: zeen_hir::HirMacroKind::Println,
                ref arg_types,
                ..
            } if arg_types.len() == 1
        )
    });
    assert!(
        println_has_single_arg,
        "println must receive a single display-result argument"
    );
}

#[test]
fn slice_struct_field_registers_slice_layout() {
    let mir = compile_mir_ok(
        "struct S { pub slc: []const char } \
         fn make() S { S { .slc = \"hi\" } } \
         fn main() { let s = make(); @println(\"{}\", s.slc); }",
    );

    // A slice field must get a synthetic `{ ptr, len }` layout; without it
    // codegen panics even when the slice points at static string data.
    let has_slice_layout = mir
        .program
        .struct_layouts
        .values()
        .any(|layout| layout.def_id == zeen_types::SLICE_STRUCT_DEF);

    assert!(
        has_slice_layout,
        "expected a registered slice layout for the struct field"
    );
}

#[test]
fn discarded_expression_results_warn() {
    let mir = compile_mir_ok(
        "fn foo() i32 { 123 } \
         fn bar() { @println(\"hi\"); } \
         fn main() { foo(); let _ = foo(); let x = foo(); x + 1; bar(); @println(\"ok\"); }",
    );

    assert_eq!(
        mir.warnings.len(),
        2,
        "expected warnings for `foo();` and `x + 1;` only, got {:?}",
        mir.warnings
    );
}

#[test]
fn explicit_discard_and_void_statements_do_not_warn() {
    let mir = compile_mir_ok(
        "fn foo() i32 { 123 } \
         fn bar() { @println(\"hi\"); } \
         fn main() { let _ = foo(); bar(); @println(\"ok\"); let x = foo(); x = x + 1; }",
    );

    assert!(
        mir.warnings.is_empty(),
        "explicit discards and void statements must not warn, got {:?}",
        mir.warnings
    );
}
