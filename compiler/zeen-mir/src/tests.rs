use std::{cell::RefCell, collections::HashSet, path::Path, rc::Rc, sync::Arc};

use bumpalo::Bump;
use lasso::Rodeo;
use zeen_driver::{CompilationContext, CompilationMode, CompilationOutput, PathsConfig};
use zeen_parser::Parser;

use crate::lowering::{MirLoweringResult, lower_program};

const CORE_OPS: &str = include_str!("../../../lib/core/ops.zn");
const CORE_OUT: &str = include_str!("../../../lib/core/out.zn");

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
        core_files: vec![("core.ops", CORE_OPS), ("core.out", CORE_OUT)],
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
    )
    .map_err(|errs| errs.iter().map(|e| e.to_string()).collect::<Vec<_>>())?;

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
         implement Display : Foo { fn display(*const self, out: OutStream) void { out.write_str(\"foo\"); } } \
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

    // The display method receives the stdout writer: the call passes the
    // receiver plus one extra argument.
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

    // println itself keeps only the literal parts (the trailing newline):
    // the struct content is written by the display call.
    let println_has_no_args = main.blocks.iter().any(|b| {
        matches!(
            b.terminator,
            crate::Terminator::MacroCall {
                kind: zeen_hir::HirMacroKind::Println,
                ref arg_types,
                ..
            } if arg_types.is_empty()
        )
    });
    assert!(
        println_has_no_args,
        "println must keep only literal parts, struct args go through display"
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

#[test]
fn global_var_lowers_to_global_place() {
    let mir = compile_mir_ok(
        "let g: i32 = 42; \
         fn main() { let x = g; }",
    );

    assert!(!mir.program.global_vars.is_empty(), "expected global vars");
    assert!(
        mir.program.init_globals_fn.is_some(),
        "expected init_globals_fn"
    );
    assert!(
        mir.program
            .function_names
            .values()
            .any(|n| n == "zeen_init_globals"),
        "expected zeen_init_globals function"
    );
}

#[test]
fn const_global_is_marked_const() {
    let mir = compile_mir_ok(
        "const c: i32 = 100; \
         fn main() {}",
    );

    let global = mir.program.global_vars.first().expect("expected global");
    assert!(global.is_const, "expected is_const true");
}

#[test]
fn global_depends_on_another_init_order() {
    let mir = compile_mir_ok(
        "let a: i32 = 1; \
         let b: i32 = a + 1; \
         fn main() {}",
    );

    let init_id = mir.program.init_globals_fn.expect("expected init fn");
    let init_fn = &mir.program.functions[&init_id];
    let stmts = &init_fn.blocks[0].statements;

    let a_idx = mir
        .program
        .global_vars
        .iter()
        .position(|g| g.symbol_name == "a")
        .expect("global a");
    let b_idx = mir
        .program
        .global_vars
        .iter()
        .position(|g| g.symbol_name == "b")
        .expect("global b");

    let a_assign_pos = stmts.iter().position(|s| {
        matches!(
            s,
            crate::MirStatement::Assign { place, .. }
            if matches!(place.projection.first(), Some(crate::PlaceElem::Global(id)) if id.0 as usize == a_idx)
        )
    });
    let b_assign_pos = stmts.iter().position(|s| {
        matches!(
            s,
            crate::MirStatement::Assign { place, .. }
            if matches!(place.projection.first(), Some(crate::PlaceElem::Global(id)) if id.0 as usize == b_idx)
        )
    });

    assert!(
        a_assign_pos < b_assign_pos,
        "a must be initialized before b"
    );
}

// --> Closures

use crate::{
    AggregateKind, CallTarget, ConstValue, LocalId, MirFunctionId, Operand, Rvalue, Terminator,
};
use zeen_types::{CLOSURE_FAT_ENV_FIELD, CLOSURE_FAT_FN_FIELD, is_closure_struct_def};

fn fn_id_by_name(mir: &MirLoweringResult, name: &str) -> Option<MirFunctionId> {
    mir.program
        .function_names
        .iter()
        .find(|(_, n)| n.as_str() == name)
        .map(|(id, _)| *id)
}

fn calls_of(mir: &MirLoweringResult, id: MirFunctionId) -> Vec<&Terminator> {
    mir.program.functions[&id]
        .blocks
        .iter()
        .map(|b| &b.terminator)
        .collect()
}

// Capturing closures lower to a fat value that *is* the captured environment
// (an inline struct of captures); the closure body gets a leading `*const`
// parameter pointing at it (env-first ABI), and call sites dispatch directly
// to the body with `&value` as that first argument.

fn closure_id_named(mir: &MirLoweringResult, name: &str) -> MirFunctionId {
    fn_id_by_name(mir, name).expect("expected closure function by name")
}

fn fn_id_starting_with(mir: &MirLoweringResult, prefix: &str) -> MirFunctionId {
    mir.program
        .function_names
        .iter()
        .find(|(_, n)| n.as_str().starts_with(prefix))
        .map(|(id, _)| *id)
        .unwrap_or_else(|| panic!("expected a function named like `{prefix}`"))
}

fn env_rooted(op: &Operand, env_local: LocalId) -> bool {
    let place = match op {
        Operand::Copy(p, _) | Operand::Move(p, _) => p,
        Operand::Constant(_, _) => return false,
    };
    place.local == env_local
        && matches!(
            place.projection.as_slice(),
            [crate::PlaceElem::Deref, crate::PlaceElem::Field(_)]
        )
}

#[test]
fn capturing_closure_body_has_env_first_param() {
    let mir = compile_mir_ok(
        "fn main() { \
             let n = 5; \
             let add = fn(x: i32) i32 { return x + n; }; \
             let r = add(10); \
             @println(\"{}\", r); \
         }",
    );

    let body_id = closure_id_named(&mir, "main->closure0");
    let body = &mir.program.functions[&body_id];

    assert_eq!(body.params.len(), 2, "expected (env, user arg) params");

    // Captured reads resolve as `(*env).$envN` rooted at the first param.
    let reads_via_env = body.blocks.iter().any(|b| {
        b.statements.iter().any(|s| {
            let crate::MirStatement::Assign { rvalue, .. } = s else {
                return false;
            };
            let mut found = false;
            if let Rvalue::BinaryOp { lhs, rhs, .. } = rvalue {
                found |= env_rooted(lhs, body.params[0]);
                found |= env_rooted(rhs, body.params[0]);
            } else if let Rvalue::Use(op) = rvalue {
                found |= env_rooted(op, body.params[0]);
            }
            found
        })
    });
    assert!(
        reads_via_env,
        "captured variable reads must go through `(*env).$envN`"
    );
}

#[test]
fn fat_call_passes_env_before_user_args() {
    let mir = compile_mir_ok(
        "fn apply(f: Fn(i32) i32, x: i32) i32 { f(x) } \
         fn main() { \
             let n = 5; \
             let add = fn(x: i32) i32 { return x + n; }; \
             let r = apply(add, 10); \
             @println(\"{}\", r); \
         }",
    );

    let apply_id = fn_id_starting_with(&mir, "apply");
    let apply = &mir.program.functions[&apply_id];

    // The fat call dispatches directly to the closure body, passing `&f`
    // (a const ref to the fat parameter) as the leading env-first argument.
    let fat_call = apply.blocks.iter().any(|b| {
        matches!(
            &b.terminator,
            Terminator::Call {
                func: CallTarget::Direct(_),
                args,
                ..
            } if matches!(
                args.first(),
                Some(Operand::Copy(place, _)) if place.projection.is_empty()
            )
        )
    });
    assert!(
        fat_call,
        "fat call must dispatch directly with `&value` as the first argument"
    );

    // The env pointer is built with a `&const` ref of the fat value itself.
    let takes_env_addr = apply.blocks.iter().any(|b| {
        b.statements.iter().any(|s| {
            matches!(
                s,
                crate::MirStatement::Assign {
                    rvalue: Rvalue::Ref { is_const: true, .. },
                    ..
                }
            )
        })
    });
    assert!(takes_env_addr, "expected a `&const value` env pointer");
}

#[test]
fn closure_values_never_touch_the_heap() {
    let mir = compile_mir_ok(
        "fn apply(f: Fn(i32) i32, x: i32) i32 { f(x) } \
         fn main() { \
             let n = 5; \
             let add = fn(x: i32) i32 { return x + n; }; \
             @println(\"{}\", apply(add, 10)); \
         }",
    );

    assert!(
        mir.program.extern_fns.is_empty(),
        "closure values are inline structs: no malloc/free must be declared"
    );

    // The captures are grouped into the fat value with a plain aggregate.
    let builds_aggregate = mir.program.functions.values().any(|func| {
        func.blocks.iter().any(|b| {
            b.statements.iter().any(|s| {
                matches!(
                    s,
                    crate::MirStatement::Assign {
                        rvalue: Rvalue::Aggregate { .. },
                        ..
                    }
                )
            })
        })
    });
    assert!(builds_aggregate, "expected the env aggregate assignment");
}

#[test]
fn stack_only_closure_env_does_not_call_malloc() {
    let mir = compile_mir_ok(
        "fn main() { \
             let n = 5; \
             let add = fn(x: i32) i32 { return x + n; }; \
             let r = add(10); \
             @println(\"{}\", r); \
         }",
    );

    assert!(
        mir.program.extern_fns.is_empty(),
        "a closure that stays on its own frame must not malloc its env"
    );

    let main_id = closure_id_named(&mir, "main");
    let main = &mir.program.functions[&main_id];
    let takes_env_addr = main.blocks.iter().any(|b| {
        b.statements.iter().any(|s| {
            matches!(
                s,
                crate::MirStatement::Assign {
                    rvalue: Rvalue::Ref { is_const: true, .. },
                    ..
                }
            )
        })
    });
    assert!(
        takes_env_addr,
        "expected a `&const env` ref for the stack env"
    );
}

#[test]
fn zero_capture_closure_coerced_to_fat_dispatches_directly() {
    let mir = compile_mir_ok(
        "fn apply_once(f: FnOnce(i32) i32, x: i32) i32 { f(x) } \
         fn main() { \
             @println(\"{}\", apply_once(fn(a: i32) i32 { return a + 1; }, 41)); \
         }",
    );

    let names: Vec<String> = mir.program.function_names.values().cloned().collect();
    assert!(
        !names.iter().any(|n| n.contains("fatadapter")),
        "adapters are gone: fat values dispatch directly, got {names:?}"
    );

    // The mono copy of `apply_once` for the zero-capture closure calls the
    // closure body directly.
    let apply_once_id = fn_id_starting_with(&mir, "apply_once");
    let apply_once = &mir.program.functions[&apply_once_id];
    let direct = apply_once.blocks.iter().any(|b| {
        matches!(
            b.terminator,
            Terminator::Call {
                func: CallTarget::Direct(_),
                ..
            }
        )
    });
    assert!(direct, "the fat call must dispatch to the closure body");
}

#[test]
fn static_fn_coerced_to_fat_dispatches_directly() {
    let mir = compile_mir_ok(
        "fn double(x: i32) i32 { x * 2 } \
         fn apply(f: Fn(i32) i32, x: i32) i32 { f(x) } \
         fn main() { @println(\"{}\", apply(double, 21)); }",
    );

    let apply_id = fn_id_starting_with(&mir, "apply");
    let apply = &mir.program.functions[&apply_id];

    // The direct call inside `apply` must target the `double` body itself.
    let double_id = fn_id_by_name(&mir, "double").expect("double body must be lowered");
    let calls_double = apply.blocks.iter().any(|b| {
        matches!(
            b.terminator,
            Terminator::Call {
                func: CallTarget::Direct(id),
                ..
            } if id == double_id
        )
    });
    assert!(
        calls_double,
        "a static fn in a fat slot must be called through its own body"
    );

    // The env envelope is built at the call site: an aggregate with no
    // captures (the static fn has an empty env).
    let main_id = fn_id_by_name(&mir, "main").expect("main missing");
    let main_fn = &mir.program.functions[&main_id];
    let empty_aggregate = main_fn.blocks.iter().any(|b| {
        b.statements.iter().any(|s| {
            matches!(
                s,
                crate::MirStatement::Assign {
                    rvalue: Rvalue::Aggregate { operands, .. },
                    ..
                } if operands.is_empty()
            )
        })
    });
    assert!(
        empty_aggregate,
        "expected an empty env aggregate at the call site"
    );
}

#[test]
fn fat_layout_matches_captures() {
    let mir = compile_mir_ok(
        "fn apply(f: Fn(i32) i32, x: i32) i32 { f(x) } \
         fn main() { \
             let n = 5; \
             let add = fn(x: i32) i32 { return x + n; }; \
             @println(\"{}\", apply(add, 10)); \
         }",
    );

    // A fat value is its environment: the layout has one field per capture.
    let fat_layouts: Vec<_> = mir
        .program
        .struct_layouts
        .values()
        .filter(|l| l.def_id == zeen_types::CLOSURE_FAT_DEF)
        .collect();
    assert!(!fat_layouts.is_empty(), "fat layout must be registered");
    assert_eq!(
        fat_layouts[0].fields.len(),
        1,
        "the fat value is the env struct: one field per capture"
    );
}

#[test]
fn runtime_bare_fn_coercion_wraps_into_pointer_fat() {
    let mir = compile_mir_ok(
        "fn apply(f: Fn(i32) i32, x: i32) i32 { f(x) } \
         fn main() { \
             let k = fn(x: i32) i32 { return x + 1; }; \
             @println(\"{}\", apply(k, 5)); \
         }",
    );

    // A basic fn value read from a variable is wrapped into a one-field fat
    // value and called indirectly.
    let main_id = fn_id_by_name(&mir, "main").expect("main missing");
    let main = &mir.program.functions[&main_id];
    let wraps_fn_ptr = main.blocks.iter().any(|b| {
        b.statements.iter().any(|s| {
            matches!(
                s,
                crate::MirStatement::Assign {
                    rvalue: Rvalue::Aggregate { operands, .. },
                    ..
                } if operands.len() == 1
            )
        })
    });
    assert!(
        wraps_fn_ptr,
        "a basic fn value must be wrapped into a one-field fat value"
    );
    let apply_id = fn_id_starting_with(&mir, "apply");
    let apply = &mir.program.functions[&apply_id];
    let indirect = apply.blocks.iter().any(|b| {
        matches!(
            b.terminator,
            Terminator::Call {
                func: CallTarget::Indirect(_),
                ..
            }
        )
    });
    assert!(
        indirect,
        "a pointer-fat value must be called indirectly through its field"
    );
}

#[test]
fn zero_capture_closure_lowered_to_fn_const_call() {
    let mir =
        compile_mir_ok("fn main() { let c = fn(a: i32) i32 { return a + 1; }; let r = c(41); }");

    let main_id = fn_id_by_name(&mir, "main").expect("main missing");
    let func = &mir.program.functions[&main_id];

    let indirect = func.blocks.iter().any(|b| {
        matches!(
            b.terminator,
            Terminator::Call {
                func: CallTarget::Indirect(_),
                ..
            }
        )
    });
    assert!(indirect, "zero-capture closure call must be indirect");

    let fn_const_stored = func.blocks.iter().any(|b| {
        b.statements.iter().any(|s| {
            matches!(
                s,
                crate::MirStatement::Assign {
                    rvalue: Rvalue::Use(Operand::Constant(ConstValue::Fn(_), _)),
                    ..
                }
            )
        })
    });
    assert!(
        fn_const_stored,
        "closure value must lower to a fn-ptr constant"
    );
}

#[test]
fn generic_typed_capture_is_rejected() {
    let errors = compile_mir("fn generic[T](v: T) void { let c = fn() T { return v; }; }")
        .err()
        .expect("generic-typed capture must be rejected");

    assert!(
        errors.iter().any(|e| e.contains("generic")),
        "expected generic capture error, got: {errors:?}"
    );
}

// A heap-env `FnOnce` closure owns its captured block: an escaping,
// non-Copy-capturing closure must get a synthesized `$fatdrop#N` free
// function, while frame-bound or copyable closures must not.

#[test]
fn escaping_fnonce_closure_gets_fat_drop_function() {
    let mir = compile_mir_ok(
        "struct Wrap { pub v: i32 } \
         fn apply_once(f: FnOnce(i32) i32, x: i32) i32 { return f(x); } \
         fn main() i32 { \
             let w = Wrap { .v = 3 }; \
             let c = fn(a: i32) i32 { return a + w.v; }; \
             return apply_once(c, 10); \
         }",
    );

    assert!(
        !mir.program
            .extern_fns
            .iter()
            .any(|f| f.symbol_name == "free"),
        "closure envs are inline: `free` must not be declared"
    );
    assert!(
        mir.program
            .function_names
            .values()
            .any(|n| n.starts_with("$fatdrop#")),
        "expected a synthesized `$fatdrop#N` drop function for the FnOnce value",
    );
}

#[test]
fn fn_closure_needs_no_drop_function() {
    let mir = compile_mir_ok(
        "fn main() { \
             let n = 5; \
             let add = fn(x: i32) i32 { return x + n; }; \
             let r = add(10); \
             @println(\"{}\", r); \
         }",
    );

    assert!(
        !mir.program
            .extern_fns
            .iter()
            .any(|f| f.symbol_name == "free"),
        "a frame-bound closure must not declare `free`: its env is on the stack"
    );
    // `Fn` values hold only Copy captures: nothing to tear down, no drop
    // function.
    assert!(
        !mir.program
            .function_names
            .values()
            .any(|n| n.starts_with("$fatdrop#")),
        "an `Fn` closure's captures are all Copy: no drop function"
    );
}

#[test]
fn fnonce_call_consumes_the_closure_value() {
    let mir = compile_mir_ok(
        "fn apply_once(f: FnOnce(i32) i32, x: i32) i32 { return f(x); } \
         fn main() i32 { \
             return apply_once(fn(a: i32) i32 { return a + 1; }, 41); \
         }",
    );

    let apply_once_id = fn_id_starting_with(&mir, "apply_once");
    let apply_once = &mir.program.functions[&apply_once_id];
    let f_param = apply_once.params[0];

    // The body must move the whole fat value into a slot before extracting
    // `$fn`/`$env`, so a second call of `f` is rejected by dataflow.
    let consumes_value = apply_once.blocks.iter().any(|b| {
        b.statements.iter().any(|s| {
            matches!(
                s,
                crate::MirStatement::Assign {
                    rvalue: Rvalue::Use(Operand::Move(place, _)),
                    ..
                } if place.local == f_param && place.projection.is_empty()
            )
        })
    });
    assert!(
        consumes_value,
        "`FnOnce` call must move the whole closure value into a slot"
    );
}

#[test]
fn live_heap_env_closure_registers_drop_function() {
    // The closure is referenced without being moved (a shared borrow keeps it
    // alive), so dataflow will drop it at scope exit and the env must be
    // released through the synthesized per-type drop function.
    let mir = compile_mir_ok(
        "struct Wrap { pub v: i32 } \
         fn main() i32 { \
             let w = Wrap { .v = 3 }; \
             let c = fn(a: i32) i32 { return a + w.v; }; \
             let p = &c; \
             return 0; \
         }",
    );

    assert!(
        mir.program
            .function_names
            .values()
            .any(|n| n.starts_with("$fatdrop#")),
        "expected a synthesized `$fatdrop#N` drop function for the FnOnce value"
    );
}

#[test]
fn fnonce_param_mono_copy_registers_drop_function() {
    // The `FnOnce(i32) i32` parameter type is erased in the signature, but
    // the monomorphized copy stores the concrete closure type and its
    // FnOnce drop function must exist for uncalled/owned values.
    let mir = compile_mir_ok(
        "fn apply_once(f: FnOnce(i32) i32, x: i32) i32 { return f(x); } \
         fn main() i32 { \
             return apply_once(fn(a: i32) i32 { return a + 1; }, 41); \
         }",
    );

    assert!(
        !mir.program
            .extern_fns
            .iter()
            .any(|f| f.symbol_name == "free"),
        "closure envs are inline: `free` must not be declared"
    );
    assert!(
        mir.program
            .function_names
            .values()
            .any(|n| n.starts_with("$fatdrop#")),
        "the concrete FnOnce fat type must get a drop function"
    );
}

#[test]
fn consuming_call_of_concrete_env_calls_drop_function() {
    // An in-frame `FnOnce` call consumes the value: the env must be released
    // right after the call, through the type's drop function.
    let mir = compile_mir_ok(
        "struct Wrap { pub v: i32 } \
         fn main() i32 { \
             let w = Wrap { .v = 3 }; \
             let c = fn(a: i32) i32 { return a + w.v; }; \
             return c(10); \
         }",
    );

    let main_id = closure_id_named(&mir, "main");
    let main = &mir.program.functions[&main_id];

    // A direct call to the synthesized drop function must follow the
    // indirect fat call.
    let drops_after_call = main.blocks.iter().any(|b| {
        matches!(
            &b.terminator,
            Terminator::Call {
                func: CallTarget::Direct(_),
                args,
                ..
            } if matches!(args.first(), Some(Operand::Copy(place, _)) if place.projection.is_empty())
        )
    });
    assert!(
        drops_after_call,
        "a consuming call must invoke the fat drop function on the consumed value"
    );
}

#[test]
fn generic_bound_method_call_dispatches_to_concrete_impl() {
    // `out.write_str(...)` where `O: StrWriter` must dispatch to the concrete
    // implementation (`MyOut.write_str`), not the bodyless interface method.
    let mir = compile_mir_ok(
        "struct MyOut {} \
         implement StrWriter : MyOut { fn write_str(*self, value: []const char) void {} } \
         fn helper[O: StrWriter](out: O) void { out.write_str(\"hi\"); } \
         fn main() { let o = MyOut {}; helper(o); }",
    );

    let names: Vec<String> = mir.program.function_names.values().cloned().collect();

    assert!(
        names.iter().any(|n| n == "MyOut.write_str"),
        "expected `MyOut.write_str` in MIR function names, got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "write_str"),
        "bare interface method `write_str` must not be emitted, got {names:?}"
    );
}
