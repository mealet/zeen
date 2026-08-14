//! Unit tests for LLVM codegen.
//!
//! The frontend pipeline lives in the `zeen` binary crate, so these tests
//! hand-build MIR with [`crate::fixtures::Fixture`] and feed it straight to
//! [`crate::CodeGen`]. Every test generates the module and runs
//! `module.verify()`, so the IR is guaranteed to be well-formed.

use std::rc::Rc;

use inkwell::context::Context;
use zeen_ast::expressions::BinaryOp;
use zeen_driver::CompilationMode;
use zeen_mir::{CallTarget, ConstValue, Operand, Rvalue, StructFieldLayout, StructLayout};
use zeen_resolve::DefKind;
use zeen_typecheck::format_str::{FormatChunk, FormatSpec};
use zeen_types::Type;

use crate::codegen::{CodeGen, CodegenOptions};
use crate::fixtures::*;

/// Runs the full codegen pipeline on a fixture and returns the printed IR.
fn compile(fx: &Fixture, mode: CompilationMode) -> String {
    let context = Context::create();
    let options = CodegenOptions {
        mode,
        main_fn: fx.main_fn,
        ..Default::default()
    };
    let mut cg = CodeGen::new(
        &context,
        &fx.program,
        &fx.typecheck,
        &fx.resolution,
        Rc::clone(&fx.rodeo),
        options,
    )
    .unwrap();
    cg.generate().unwrap();
    cg.verify().unwrap();
    cg.print_ir()
}

fn main_returns_i32(fx: &mut Fixture, value: i128) {
    let main_def = fx.def("main", DefKind::Function);
    let i32 = fx.i32();
    let mut main = fx.fn_builder("main", main_def, i32);
    let ret = main.temp(i32);
    main.entry("bb0");
    main.assign(place(ret), use_const(const_int(value)));
    main.ret(copy_of(ret));
    main.finish();
}

#[test]
fn main_wrapper_calls_zeen_main_and_returns_value() {
    let mut fx = Fixture::new();
    main_returns_i32(&mut fx, 42);

    let ir = compile(&fx, CompilationMode::Debug);

    assert!(ir.contains("define i32 @zeen_main()"), "{ir}");
    assert!(ir.contains("define i32 @main()"), "{ir}");
    assert!(ir.contains("call i32 @zeen_main()"), "{ir}");
}

#[test]
fn main_wrapper_void_returns_zero() {
    let mut fx = Fixture::new();
    let main_def = fx.def("main", DefKind::Function);
    let void = fx.void();
    let mut main = fx.fn_builder("main", main_def, void);
    main.entry("bb0");
    main.ret_void();
    main.finish();

    let ir = compile(&fx, CompilationMode::Debug);

    assert!(ir.contains("define void @zeen_main()"), "{ir}");
    assert!(ir.contains("ret i32 0"), "{ir}");
}

#[test]
fn main_wrapper_sign_extends_smaller_returns() {
    let mut fx = Fixture::new();
    let main_def = fx.def("main", DefKind::Function);
    let i8 = fx.i8();
    let mut main = fx.fn_builder("main", main_def, i8);
    let ret = main.temp(i8);
    main.entry("bb0");
    main.assign(place(ret), use_const(const_int(-1)));
    main.ret(copy_of(ret));
    main.finish();

    let ir = compile(&fx, CompilationMode::Debug);

    assert!(ir.contains("define i8 @zeen_main()"), "{ir}");
    assert!(ir.contains("sext i8"), "{ir}");
}

#[test]
fn no_main_produces_no_wrapper() {
    let mut fx = Fixture::new();
    let add_def = fx.def("add", DefKind::Function);
    let i32 = fx.i32();
    let mut f = fx.fn_builder("add", add_def, i32);
    let a = f.param("a", i32);
    let b = f.param("b", i32);
    let ret = f.temp(i32);
    f.entry("bb0");
    f.assign(place(ret), binary(BinaryOp::Add, copy_of(a), copy_of(b)));
    f.ret(copy_of(ret));
    f.finish();

    let ir = compile(&fx, CompilationMode::Debug);

    assert!(ir.contains("define i32 @add("), "{ir}");
    assert!(ir.contains("add i32"), "{ir}");
    assert!(!ir.contains("define i32 @main"), "{ir}");
}

#[test]
fn panic_emits_runtime_call_and_unreachable() {
    let mut fx = Fixture::new();
    let panic_def = fx.def("boom", DefKind::Function);
    let void = fx.void();
    let mut f = fx.fn_builder("boom", panic_def, void);
    let dest = f.temp(void);
    f.entry("bb0");
    f.panic("boom", vec![], dest);
    f.finish();

    let ir = compile(&fx, CompilationMode::Debug);

    assert!(ir.contains("@printf"), "{ir}");
    assert!(ir.contains("@exit"), "{ir}");
    assert!(ir.contains("boom"), "{ir}");
    assert!(ir.contains("unreachable"), "{ir}");
}

#[test]
fn print_emits_printf_call() {
    let mut fx = Fixture::new();
    let print_def = fx.def("print", DefKind::Function);
    let void = fx.void();
    let mut f = fx.fn_builder("print", print_def, void);
    let dest = f.temp(void);
    f.entry("bb0");
    f.block("bb1");
    f.set_current("bb0");
    f.println("hello, zeen", vec![], dest, Some("bb1"));
    f.set_current("bb1");
    f.ret_void();
    f.finish();

    let ir = compile(&fx, CompilationMode::Debug);

    assert!(ir.contains("@printf"), "{ir}");
    assert!(ir.contains("hello, zeen"), "{ir}");
}

#[test]
fn format_returns_a_slice() {
    let mut fx = Fixture::new();
    let fmt_def = fx.def("fmt", DefKind::Function);
    let void = fx.void();

    let char = fx.char();
    let slice = fx.slice(char);

    // Register the synthetic `[]T` layout: `{ ptr: [*]T, len: usize }`.
    let ptr_ty = fx.ty(Type::ManyPointer {
        inner: char,
        is_const: false,
    });
    let usize_ty = fx.usize();
    fx.add_struct_layout(
        slice,
        StructLayout {
            def_id: zeen_types::SLICE_STRUCT_DEF,
            generic_args: vec![char],
            fields: vec![
                StructFieldLayout {
                    def_id: zeen_types::SLICE_PTR_FIELD,
                    ty: ptr_ty,
                },
                StructFieldLayout {
                    def_id: zeen_types::SLICE_LEN_FIELD,
                    ty: usize_ty,
                },
            ],
        },
    );

    let mut f = fx.fn_builder("fmt", fmt_def, void);
    let dest = f.temp(slice);
    f.entry("bb0");
    f.block("bb1");
    f.set_current("bb0");
    f.format(
        vec![
            FormatChunk::Literal("value is ".to_string()),
            FormatChunk::Arg(FormatSpec::Display),
        ],
        vec![const_int(42)],
        dest,
        Some("bb1"),
    );
    f.set_current("bb1");
    f.ret_void();
    f.finish();

    let ir = compile(&fx, CompilationMode::Debug);

    assert!(ir.contains("@snprintf"), "{ir}");
    assert!(ir.contains("@sprintf"), "{ir}");
    assert!(ir.contains("%slice.char"), "{ir}");
}

#[test]
fn extern_fn_call_uses_declared_symbol() {
    let mut fx = Fixture::new();
    let void = fx.void();
    let ptr_void = fx.ptr(void);
    let i32 = fx.i32();
    fx.add_extern_fn("puts", vec![ptr_void], i32, false);
    let msg = const_str(&mut fx, "hi");

    let ex_def = fx.def("ex", DefKind::Function);
    let mut f = fx.fn_builder("ex", ex_def, i32);
    let ret = f.temp(i32);
    f.entry("bb0");
    f.block("bb1");
    f.set_current("bb0");
    f.call(CallTarget::Extern(0), vec![msg], ret, Some("bb1"));
    f.set_current("bb1");
    f.ret(copy_of(ret));
    f.finish();

    let ir = compile(&fx, CompilationMode::Debug);

    assert!(ir.contains("declare i32 @puts"), "{ir}");
    assert!(ir.contains("call i32 @puts"), "{ir}");
}

#[test]
fn struct_field_store_and_load() {
    let mut fx = Fixture::new();
    let struct_def = fx.def("Pair", DefKind::Struct);
    let field_def = fx.def("x", DefKind::Field);
    let i32 = fx.i32();
    let struct_ty = fx.ty(Type::Struct {
        def_id: struct_def,
        generic_args: Vec::new(),
    });
    fx.add_struct_layout(
        struct_ty,
        StructLayout {
            def_id: struct_def,
            generic_args: Vec::new(),
            fields: vec![StructFieldLayout {
                def_id: field_def,
                ty: i32,
            }],
        },
    );

    let read_def = fx.def("read", DefKind::Function);
    let mut f = fx.fn_builder("read", read_def, i32);
    let s = f.local("s", struct_ty);
    let ret = f.temp(i32);
    f.entry("bb0");
    f.assign(place(s).field(field_def), use_const(const_int(7)));
    let field = Operand::Copy(place(s).field(field_def), None);
    f.assign(place(ret), use_const(field));
    f.ret(copy_of(ret));
    f.finish();

    let ir = compile(&fx, CompilationMode::Debug);

    assert!(ir.contains("getelementptr"), "{ir}");
}

#[test]
fn release_mode_runs_optimization_passes() {
    let mut fx = Fixture::new();
    main_returns_i32(&mut fx, 21);

    let ir = compile(&fx, CompilationMode::Release);

    // The optimizer may annotate the definitions (`noundef`, `local_unnamed_addr`).
    assert!(ir.contains("@zeen_main()"), "{ir}");
    assert!(ir.contains("@main()"), "{ir}");
}

#[test]
fn const_string_global_is_deduplicated() {
    let mut fx = Fixture::new();
    let main_def = fx.def("main", DefKind::Function);
    let i32 = fx.i32();
    let same = const_str(&mut fx, "same");
    let mut main = fx.fn_builder("main", main_def, i32);
    let ret = main.temp(i32);
    main.entry("bb0");
    main.assign(place(ret), use_const(same));
    main.ret(copy_of(ret));
    main.finish();

    let ir = compile(&fx, CompilationMode::Debug);

    assert!(ir.contains("@str.0"), "{ir}");
    assert!(!ir.contains("@str.1"), "{ir}");
}

fn const_null() -> Operand {
    Operand::Constant(ConstValue::NullPtr, None)
}

#[test]
fn pointer_equality_and_inequality_compare_as_integers() {
    let mut fx = Fixture::new();
    let main_def = fx.def("main", DefKind::Function);
    let i32 = fx.i32();
    let bool_ty = fx.bool();
    let ptr_ty = fx.ptr(i32);

    let mut main = fx.fn_builder("main", main_def, i32);
    let p = main.local("p", ptr_ty);
    let eq = main.temp(bool_ty);
    let ne = main.temp(bool_ty);
    let ret = main.temp(i32);

    main.entry("bb0");
    main.assign(place(p), use_const(const_null()));
    main.assign(place(eq), binary(BinaryOp::Eq, copy_of(p), const_null()));
    main.assign(place(ne), binary(BinaryOp::Ne, copy_of(p), const_null()));
    main.assign(
        place(ret),
        Rvalue::Cast {
            operand: Operand::Copy(place(eq), None),
            target: i32,
        },
    );
    main.ret(copy_of(ret));
    main.finish();

    let ir = compile(&fx, CompilationMode::Debug);

    assert!(ir.contains("ptrtoint"), "{ir}");
    assert!(ir.contains("icmp eq"), "{ir}");
    assert!(ir.contains("icmp ne"), "{ir}");
}
