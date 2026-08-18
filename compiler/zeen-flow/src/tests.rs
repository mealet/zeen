use std::{cell::RefCell, collections::HashSet, path::Path, rc::Rc, sync::Arc};

use bumpalo::Bump;
use lasso::Rodeo;
use zeen_driver::{CompilationContext, CompilationMode, CompilationOutput, PathsConfig};
use zeen_parser::Parser;

use crate::{error::FlowError, run_dataflow};

const CORE_OPS: &str = include_str!("../../../lib/core/ops.zn");

fn flow_errors(src: &str) -> Vec<FlowError> {
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

    let program = parser
        .parse_program()
        .map_err(|errs| errs.iter().map(|e| e.to_string()).collect::<Vec<_>>())
        .expect("parse must succeed");

    let (resolved_program, mut resolution_result) = zeen_resolve::resolve(
        Rc::clone(&filename),
        Arc::clone(&content),
        Path::new("/test.zn"),
        program,
        &bump,
        Rc::clone(&rodeo),
        &mut context,
    )
    .map_err(|errs| errs.iter().map(|e| e.to_string()).collect::<Vec<_>>())
    .expect("resolve must succeed");

    let mut hir_lowering = zeen_hir::HirLowering::new(&resolution_result, Rc::clone(&rodeo));
    let hir_module = hir_lowering.lower_module(resolved_program);

    let mut typechecker =
        zeen_typecheck::TypeChecker::new(&mut resolution_result, &context, Rc::clone(&rodeo));
    typechecker.check_module(&hir_module);

    let mut typecheck = typechecker
        .finish()
        .map_err(|errs| errs.iter().map(|e| e.to_string()).collect::<Vec<_>>())
        .expect("typecheck must succeed");

    let mut lowered_mir = zeen_mir::lowering::lower_program(
        Rc::clone(&rodeo),
        &mut typecheck,
        &resolution_result,
        &hir_module,
        context.mode,
    );

    match run_dataflow(
        &mut lowered_mir.program,
        &mut typecheck,
        &resolution_result,
        Rc::clone(&rodeo),
    ) {
        Ok(result) => result.errors,
        Err(errors) => errors,
    }
}

fn has_escaping_borrow(errors: &[FlowError]) -> bool {
    errors
        .iter()
        .any(|e| matches!(e, FlowError::EscapingBorrow { .. }))
}

#[test]
fn returning_slice_of_local_is_rejected() {
    let errors = flow_errors(
        "fn make() []const i32 { \
             let a: [4]i32 = [1, 2, 3, 4]; \
             return &a; \
         } \
         fn main() { let s = make(); @println(\"{}\", s[0]); }",
    );

    assert!(
        has_escaping_borrow(&errors),
        "expected escaping-borrow error, got {errors:?}"
    );
}

#[test]
fn returning_slice_of_local_through_temp_is_rejected() {
    let errors = flow_errors(
        "fn make() []const i32 { \
             let a: [4]i32 = [1, 2, 3, 4]; \
             let s: []const i32 = &a; \
             return s; \
         } \
         fn main() { let s = make(); @println(\"{}\", s[0]); }",
    );

    assert!(
        has_escaping_borrow(&errors),
        "expected escaping-borrow error, got {errors:?}"
    );
}

#[test]
fn returning_struct_with_local_slice_field_is_rejected() {
    let errors = flow_errors(
        "struct S { pub slc: []const i32 } \
         fn make() S { \
             let a: [4]i32 = [1, 2, 3, 4]; \
             let s: []const i32 = &a; \
             return S { .slc = s }; \
         } \
         fn main() { let o = make(); @println(\"{}\", o.slc[0]); }",
    );

    assert!(
        has_escaping_borrow(&errors),
        "expected escaping-borrow error, got {errors:?}"
    );
}

#[test]
fn returning_string_literal_slice_is_allowed() {
    let errors = flow_errors(
        "fn greet() []const char { \"hello\" } \
         fn main() { @println(\"{}\", greet()); }",
    );

    assert!(
        !has_escaping_borrow(&errors),
        "unexpected escaping-borrow error, got {errors:?}"
    );
}

#[test]
fn returning_param_slice_is_allowed() {
    let errors = flow_errors(
        "fn pass(s: []const char) []const char { s } \
         fn main() { @println(\"{}\", pass(\"hi\")); }",
    );

    assert!(
        !has_escaping_borrow(&errors),
        "unexpected escaping-borrow error, got {errors:?}"
    );
}
