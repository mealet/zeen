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
