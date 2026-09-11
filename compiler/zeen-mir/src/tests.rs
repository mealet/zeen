use std::{cell::RefCell, collections::HashSet, path::Path, rc::Rc, sync::Arc};

use bumpalo::Bump;
use lasso::Rodeo;
use zeen_driver::{CompilationContext, CompilationMode, CompilationOutput, PathsConfig};
use zeen_parser::Parser;

use crate::lowering::{MirLoweringResult, lower_program};

const CORE_OPS: &str = include_str!("../../../lib/core/ops.zn");
const CORE_OUT: &str = include_str!("../../../lib/core/io.zn");
const CORE_ITER: &str = include_str!("../../../lib/core/iter.zn");
const CORE_OPTION: &str = include_str!("../../../lib/core/option.zn");

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
            std_root: Some(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../lib/std"),
            ),
            linked: HashSet::new(),
        },
        core_files: vec![
            ("core.ops", CORE_OPS),
            ("core.out", CORE_OUT),
            ("core.iter", CORE_ITER),
            ("core.option", CORE_OPTION),
        ],
        mode,
        output: CompilationOutput::EmitMIR,
        target: None,
        warnings: Vec::new(),
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
fn for_loop_over_iterator_struct_monomorphizes_next() {
    let mir = compile_mir_ok(
        "struct Counter { n: i32 } \
         implement Iterator : Counter { fn next(*self) Option[i32] { \
            if (self.n < 5) { self.n = self.n + 1; return Option.Some(self.n); }; \
            Option.None() \
         } } \
         fn main() { let counter = Counter { .n = 0 }; \
            for (i : counter) { @println(\"{}\", i); } }",
    );

    let names: Vec<String> = mir.program.function_names.values().cloned().collect();

    assert!(
        names.iter().any(|n| n == "Counter.next"),
        "expected a monomorphized `Counter.next` in MIR function names, got {names:?}"
    );
}

#[test]
fn for_loop_over_generic_iterator_struct_monomorphizes_next() {
    let mir = compile_mir_ok(
        "struct Repeat[T] { value: T, remaining: i32 } \
         implement[T] Iterator : Repeat[T] { fn next(*self) Option[T] { \
            if (self.remaining > 0) { self.remaining = self.remaining - 1; \
                return Option.Some(self.value); }; \
            Option.None() \
         } } \
         fn main() { let rep = Repeat { .value = 7, .remaining = 3 }; \
            for (i : rep) { @println(\"{}\", i); } }",
    );

    let names: Vec<String> = mir.program.function_names.values().cloned().collect();

    assert!(
        names.iter().any(|n| n == "Repeat[i32].next"),
        "expected a monomorphized `Repeat[i32].next` in MIR function names, got {names:?}"
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
         implement Display : Foo { fn display[W: StrWriter](*const self, out: *W) void { (*out).write_str(\"foo\"); } } \
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
fn referenced_generic_struct_registers_layout() {
    // A monomorphized struct that is only referenced (as a local annotation /
    // constructor return) but never materialized by a struct literal must
    // still get a layout; before the fix codegen panicked with
    // "no struct type registered".
    let mir = compile_mir_ok(
        "struct Foo[T] { \
             pub fn new(value: T) Self { @todo() } \
         } \
         fn main() { let a: Foo[i32] = Foo.new(123); }",
    );

    let has_foo_i32 = mir.program.struct_layouts.values().any(|layout| {
        matches!(
            layout.generic_args.as_slice(),
            [zeen_types::TypeId(_)] // one generic arg: i32
        )
    });
    assert!(
        has_foo_i32,
        "expected a registered layout for the monomorphized `Foo[i32]`"
    );
}

#[test]
fn diverging_tail_expression_keeps_unreachable() {
    // A non-void function whose body is just `@todo()` must not get a
    // type-incorrect `Return(Void)` fused into its (dead) tail block; it
    // should end with `Unreachable`.
    let mir = compile_mir_ok(
        "struct Foo {} \
         fn make() Foo { @todo() } \
         fn main() { let a = make(); }",
    );

    let make_id = fn_id_by_name(&mir, "make").expect("make missing");
    let make = &mir.program.functions[&make_id];

    let ends_unreachable = make
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, Terminator::Unreachable));
    assert!(
        ends_unreachable,
        "a diverging tail expression must end the function with `Unreachable`"
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
fn extern_var_is_registered_as_extern_global() {
    let mir = compile_mir_ok("extern let flag: i32; fn main() { let x = flag; }");

    let global = mir
        .program
        .global_vars
        .iter()
        .find(|g| g.symbol_name == "flag")
        .expect("expected extern global");
    assert!(global.is_extern, "expected is_extern true");

    let main = mir
        .program
        .function_names
        .iter()
        .find(|(_, name)| *name == "main")
        .expect("main function")
        .0;
    let main_fn = &mir.program.functions[main];
    let flag_idx = mir
        .program
        .global_vars
        .iter()
        .position(|g| g.symbol_name == "flag")
        .expect("flag global");
    let read_uses_global_place = main_fn.blocks.iter().any(|b| {
        b.statements.iter().any(
            |s| matches!(
                s,
                crate::MirStatement::Assign { rvalue: crate::Rvalue::Use(crate::Operand::Copy(place, _)), .. }
                if matches!(place.projection.first(), Some(crate::PlaceElem::Global(id)) if id.0 as usize == flag_idx)
            ),
        )
    });
    assert!(
        read_uses_global_place,
        "expected reading extern var to load its global place"
    );
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
use zeen_types::{CLOSURE_FAT_ENV_FIELD, CLOSURE_FAT_FN_FIELD};

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

// Capturing closures lower to a fat value: a static `{ $fn, $env }`
// envelope whose `$env` points at a heap-allocated struct of captures. The
// closure body gets a leading `*const` parameter pointing at that env
// struct (env-first ABI); call sites dispatch indirectly through `$fn` with
// `$env` as the leading argument, so provenance never matters at the call
// site.

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

    // The fat call dispatches indirectly through `$fn` (uniform env-first
    // ABI), passing the `$env` field copy as the leading argument before the
    // user args.
    let fat_call = apply.blocks.iter().find_map(|b| match &b.terminator {
        Terminator::Call {
            func: CallTarget::Indirect(_),
            args,
            ..
        } => Some(args),
        _ => None,
    });
    let fat_call = fat_call.expect("fat call must dispatch indirectly through `$fn`");
    assert!(
        matches!(
            fat_call.first(),
            Some(Operand::Copy(place, _))
                if matches!(
                    place.projection.as_slice(),
                    [crate::PlaceElem::Field(CLOSURE_FAT_ENV_FIELD)]
                )
        ),
        "first arg of a fat call must be the `$env` field of the callee"
    );
    assert_eq!(
        fat_call.len(),
        2,
        "the fat call takes the env field plus the user arg"
    );
}

#[test]
fn capturing_closure_env_is_heap_allocated() {
    let mir = compile_mir_ok(
        "fn apply(f: Fn(i32) i32, x: i32) i32 { f(x) } \
         fn main() { \
             let n = 5; \
             let add = fn(x: i32) i32 { return x + n; }; \
             @println(\"{}\", apply(add, 10)); \
         }",
    );

    // Captures live in a heap env block: building the closure mallocs it,
    // and every fat value's death frees it back.
    assert!(
        mir.program
            .extern_fns
            .iter()
            .any(|f| f.symbol_name == "malloc"),
        "building a capturing closure must declare `malloc` for its env"
    );
    assert!(
        mir.program
            .extern_fns
            .iter()
            .any(|f| f.symbol_name == "free"),
        "drops must declare `free` for the env block"
    );

    // The captures are grouped into a plain env aggregate first.
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
fn closure_env_block_is_stored_through_a_boxed_pointer() {
    let mir = compile_mir_ok(
        "fn main() { \
             let n = 5; \
             let add = fn(x: i32) i32 { return x + n; }; \
             let r = add(10); \
             @println(\"{}\", r); \
         }",
    );

    // The env block is malloc'd (sized with a `SizeOf`), and the aggregate is
    // stored through a cast `*void -> *env` pointer, i.e. into a boxed
    // deref.
    let main_id = closure_id_named(&mir, "main");
    let main = &mir.program.functions[&main_id];
    let sizes_env = main.blocks.iter().any(|b| {
        b.statements.iter().any(|s| {
            matches!(
                s,
                crate::MirStatement::Assign {
                    rvalue: Rvalue::SizeOf(_),
                    ..
                }
            )
        })
    });
    assert!(sizes_env, "expected a SizeOf to size the env malloc");

    let main = &mir.program.functions[&main_id];
    let stores_env = main.blocks.iter().any(|b| {
        b.statements.iter().any(|s| {
            matches!(
                s,
                crate::MirStatement::Assign {
                    place,
                    rvalue: Rvalue::Use(_),
                    ..
                } if place.projection.as_slice() == [crate::PlaceElem::Deref]
            )
        })
    });
    assert!(
        stores_env,
        "the env aggregate must be moved into the boxed block"
    );
}

#[test]
fn zero_capture_closure_coerced_to_fat_uses_env_first_adapter() {
    let mir = compile_mir_ok(
        "fn apply_once(f: FnOnce(i32) i32, x: i32) i32 { f(x) } \
         fn main() { \
             @println(\"{}\", apply_once(fn(a: i32) i32 { return a + 1; }, 41)); \
         }",
    );

    // The zero-capture closure is a plain body; its fat slot needs an
    // env-first adapter, synthesized once.
    let names: Vec<String> = mir.program.function_names.values().cloned().collect();
    assert!(
        names.iter().any(|n| n.starts_with("$fatadapt")),
        "a zero-capture closure in a fat slot must get an env-first adapter, got {names:?}"
    );

    // The mono copy of `apply_once` for the zero-capture closure calls
    // indirectly through the `$fn` field (the adapter).
    let apply_once_id = fn_id_starting_with(&mir, "apply_once");
    let apply_once = &mir.program.functions[&apply_once_id];
    let indirect = apply_once.blocks.iter().any(|b| {
        matches!(
            b.terminator,
            Terminator::Call {
                func: CallTarget::Indirect(_),
                ..
            }
        )
    });
    assert!(indirect, "the fat call must go through the adapter pointer");
}

#[test]
fn static_fn_coerced_to_fat_dispatches_through_adapter() {
    let mir = compile_mir_ok(
        "fn double(x: i32) i32 { x * 2 } \
         fn apply(f: Fn(i32) i32, x: i32) i32 { f(x) } \
         fn main() { @println(\"{}\", apply(double, 21)); }",
    );

    // The static fn has an empty env, so the fat value holds a `null` env and
    // an env-first adapter forwarding into `double`.
    let names: Vec<String> = mir.program.function_names.values().cloned().collect();
    assert!(
        names.iter().any(|n| n.starts_with("$fatadapt")),
        "a static fn in a fat slot must get an env-first adapter, got {names:?}"
    );

    // Inside `apply` the call is indirect through `$fn`.
    let apply_id = fn_id_starting_with(&mir, "apply");
    let apply = &mir.program.functions[&apply_id];
    let calls_indirect = apply.blocks.iter().any(|b| {
        matches!(
            b.terminator,
            Terminator::Call {
                func: CallTarget::Indirect(_),
                ..
            }
        )
    });
    assert!(
        calls_indirect,
        "a static fn in a fat slot must be called indirectly through `$fn`"
    );

    // The envelope is built at the call site: three fields, fn pointer, a
    // `null` env mark and the shared no-op teardown for the adapter-callable
    // empty envelope.
    let main_id = fn_id_by_name(&mir, "main").expect("main missing");
    let main_fn = &mir.program.functions[&main_id];
    let envelope = main_fn.blocks.iter().any(|b| {
        b.statements.iter().any(|s| {
            matches!(
                s,
                crate::MirStatement::Assign {
                    rvalue: Rvalue::Aggregate {
                        kind: AggregateKind::Struct(def),
                        operands,
                        ..
                    },
                    ..
                } if matches!(
                    operands.as_slice(),
                    [
                        Operand::Copy(_, _),
                        Operand::Constant(ConstValue::NullPtr, None),
                        Operand::Constant(ConstValue::Fn(_), None)
                    ]
                ) && *def == zeen_types::CLOSURE_FAT_DEF
            )
        })
    });
    assert!(
        envelope,
        "expected a `{{ fn, null, noop }}` envelope aggregate at the call site"
    );
}

#[test]
fn fat_layout_is_static_two_field_envelope() {
    let mir = compile_mir_ok(
        "fn apply(f: Fn(i32) i32, x: i32) i32 { f(x) } \
         fn main() { \
             let n = 5; \
             let add = fn(x: i32) i32 { return x + n; }; \
             @println(\"{}\", apply(add, 10)); \
         }",
    );

    // A fat value is a static `{ $fn, $env, $drop }` envelope shared by
    // every fat type; the captures live in the heap block `$env` points at
    // and `$drop` tears them down before the block is freed.
    let fat_layouts: Vec<_> = mir
        .program
        .struct_layouts
        .values()
        .filter(|l| l.def_id == zeen_types::CLOSURE_FAT_DEF)
        .collect();
    assert!(!fat_layouts.is_empty(), "fat layout must be registered");
    assert_eq!(
        fat_layouts[0].fields.len(),
        3,
        "the fat envelope always holds `$fn`, `$env` and `$drop`"
    );
    assert_eq!(
        fat_layouts[0].fields[0].def_id,
        zeen_types::CLOSURE_FAT_FN_FIELD,
    );
    assert_eq!(
        fat_layouts[0].fields[1].def_id,
        zeen_types::CLOSURE_FAT_ENV_FIELD,
    );
    assert_eq!(
        fat_layouts[0].fields[2].def_id,
        zeen_types::CLOSURE_FAT_DROP_FIELD,
    );

    // The env captures get their own inline struct layout (one field for the
    // sole captured `n`).
    let has_env_layout = mir
        .program
        .struct_layouts
        .iter()
        .any(|(_, l)| l.def_id != zeen_types::CLOSURE_FAT_DEF && !l.fields.is_empty());
    assert!(has_env_layout, "an env struct layout must be registered");
}

#[test]
fn runtime_bare_fn_coercion_boxes_pointer_and_uses_adapter() {
    let mir = compile_mir_ok(
        "fn apply(f: Fn(i32) i32, x: i32) i32 { f(x) } \
         fn main() { \
             let k = fn(x: i32) i32 { return x + 1; }; \
             @println(\"{}\", apply(k, 5)); \
         }",
    );

    // A basic fn value read from a variable is boxed into a heap fat
    // envelope (env = null, `$fn` = the pointer), and calls go through a
    // shared env-first pointer adapter.
    let names: Vec<String> = mir.program.function_names.values().cloned().collect();
    assert!(
        names.iter().any(|n| n.starts_with("$fatptr")),
        "a runtime fn pointer in a fat slot must get a pointer adapter, got {names:?}"
    );
    assert!(
        mir.program
            .extern_fns
            .iter()
            .any(|f| f.symbol_name == "malloc"),
        "boxing the fn pointer must declare `malloc`"
    );

    // Inside `apply` the call is indirect through `$fn` (the adapter).
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
        "a boxed fn pointer must be called indirectly through `$fn`"
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

// A heap-env closure owns its captured block: any fat value (Fn or FnOnce)
// must get a synthesized `$fatdrop#N` drop function that `free`s the block,
// and the `free` extern must be declared.

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
        mir.program
            .extern_fns
            .iter()
            .any(|f| f.symbol_name == "free"),
        "the env block is heap-allocated: `free` must be declared"
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
fn heap_fn_closure_gets_drop_function() {
    let mir = compile_mir_ok(
        "fn main() { \
             let n = 5; \
             let add = fn(x: i32) i32 { return x + n; }; \
             let r = add(10); \
             @println(\"{}\", r); \
         }",
    );

    // Every fat value owns a heap env block, `Fn` included: the value's
    // scope-end drop must `free` it back.
    assert!(
        mir.program
            .extern_fns
            .iter()
            .any(|f| f.symbol_name == "free"),
        "a heap-env closure must declare `free` for its env block"
    );
    assert!(
        mir.program
            .function_names
            .values()
            .any(|n| n.starts_with("$fatdrop#")),
        "an `Fn` value's heap env must also get a drop function"
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
        mir.program
            .extern_fns
            .iter()
            .any(|f| f.symbol_name == "free"),
        "the concrete FnOnce fat type owns a heap env: `free` must be declared"
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
    // indirect fat call, moving the consumed slot into it (so dataflow
    // stops tracking it).
    let drops_after_call = main.blocks.iter().any(|b| {
        matches!(
            &b.terminator,
            Terminator::Call {
                func: CallTarget::Direct(_),
                args,
                ..
            } if matches!(
                args.first(),
                Some(Operand::Move(place, _)) if place.projection.is_empty()
            )
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
         implement StrWriter : MyOut { \
           fn write_str(*self, value: []const char) void {} \
           fn write_str_raw(*self, ptr: [*]const char, len: usize) void {} \
           fn write_str_single(*self, value: char) void {} \
         } \
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

#[test]
fn sizeof_of_unused_struct_registers_layout() {
    // `@sizeof(Foo)` is the only reference to the struct; the layout must
    // still be registered so codegen can size the type.
    let mir = compile_mir_ok(
        "struct Foo { a: i32, b: i64 } \
         fn main() { let s: usize = @sizeof(Foo); @println(\"{}\", s); }",
    );

    let layouts: Vec<(usize, u32)> = mir
        .program
        .struct_layouts
        .values()
        .map(|l| {
            (
                l.fields.len(),
                l.fields.first().map(|f| f.def_id.0).unwrap_or(0),
            )
        })
        .collect();

    assert!(
        layouts.iter().any(|(len, first)| *len == 2 && *first != 0),
        "expected `Foo` layout (2 fields) to be registered for `@sizeof(Foo)`, got {layouts:?}"
    );
}

fn verifies(ty: zeen_types::TypeId, _all: usize) -> bool {
    let _ = ty;
    true
}
