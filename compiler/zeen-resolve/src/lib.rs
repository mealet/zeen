use bumpalo::Bump;
use lasso::Rodeo;
use miette::NamedSource;

use std::{cell::RefCell, path::Path, rc::Rc, sync::Arc};

use error::ResolveError;
use resolvers::{include_resolver, name_resolver};

use zeen_ast::Declaration;
use zeen_driver::CompilationContext;

pub use resolution::{
    BindingSlotKey, DefId, DefInfo, DefKind, NodeKey, Resolution, ResolutionResult,
};

mod error;
mod resolution;
mod resolvers;
mod symbol_table;

type ResolvedProgram<'ctx> = (&'ctx [&'ctx Declaration<'ctx>], ResolutionResult);

pub fn resolve<'ctx>(
    filename: Rc<String>,
    src: Arc<String>,

    entry_path: &Path,
    entry_program: &'ctx [&'ctx Declaration<'_>],

    arena: &'ctx Bump,
    interner: Rc<RefCell<Rodeo>>,
    context: &'ctx mut CompilationContext,
) -> Result<ResolvedProgram<'ctx>, Vec<ResolveError>> {
    let core_files = context.core_files.clone();

    let target = context
        .target
        .as_deref()
        .map(zeen_driver::Target::parse)
        .unwrap_or_else(zeen_driver::Target::host);
    let mode = context.mode;

    let mut include_resolver = include_resolver::IncludeResolver::new(
        Rc::clone(&filename),
        Arc::clone(&src),
        arena,
        Rc::clone(&interner),
        context,
        target,
        mode,
    );

    let resolved_core_injections = include_resolver.resolve_core_injects(
        entry_path.to_path_buf(),
        entry_program,
        miette::NamedSource::new(filename.as_str(), Arc::clone(&src)),
        &core_files,
    )?;

    let resolved_program = include_resolver.resolve(
        entry_path.to_path_buf(),
        resolved_core_injections,
        miette::NamedSource::new(filename.as_str(), Arc::clone(&src)),
    )?;

    let mut name_resolver = name_resolver::NameResolver::new(filename, src, interner);
    name_resolver.resolve_module(resolved_program);

    let resolution_result = name_resolver.finish()?;

    Ok((resolved_program, resolution_result))
}

pub fn same_source_file(a: &NamedSource<Arc<String>>, b: &NamedSource<Arc<String>>) -> bool {
    // TODO: Needs improvement, maybe mark each source with its own ID and verify it.
    a.name() == b.name()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ResolveError;
    use lasso::Rodeo;
    use std::{
        cell::RefCell,
        collections::HashSet,
        path::{Path, PathBuf},
        rc::Rc,
        sync::Arc,
    };
    use zeen_driver::{CompilationMode, CompilationOutput, PathsConfig};
    use zeen_parser::Parser;

    const CORE_OPS: &str = include_str!("../../../lib/core/ops.zn");
    const CORE_OUT: &str = include_str!("../../../lib/core/io.zn");

    #[derive(Debug)]
    struct Fixture {
        rodeo: Rc<RefCell<Rodeo>>,
        resolution: ResolutionResult,
    }

    impl Fixture {
        fn name(&self, def: &DefInfo) -> String {
            self.rodeo.borrow().resolve(&def.name).to_string()
        }

        fn find_def(&self, name: &str) -> Option<DefInfo> {
            self.resolution
                .defs
                .values()
                .find(|def| self.name(def) == name)
                .cloned()
        }
    }

    fn resolve_full(src: &str) -> Result<Fixture, Vec<ResolveError>> {
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
            core_files: vec![("core.ops", CORE_OPS), ("core.out", CORE_OUT)],
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
        let program = parser.parse_program().map_err(|errs| {
            errs.iter()
                .map(|e| ResolveError::ModuleParseError(e.clone()))
                .collect::<Vec<_>>()
        })?;

        let target = context
            .target
            .as_deref()
            .map(zeen_driver::Target::parse)
            .unwrap_or_else(zeen_driver::Target::host);
        let program = zeen_preprocessor::resolve(program, &bump, &rodeo, &target, context.mode);

        let lookup_rodeo = Rc::clone(&rodeo);

        resolve(
            Rc::clone(&filename),
            Arc::clone(&content),
            Path::new("/test.zn"),
            program,
            &bump,
            rodeo,
            &mut context,
        )
        .map(|(_, resolution_result)| Fixture {
            rodeo: lookup_rodeo,
            resolution: resolution_result,
        })
    }

    fn resolve_ok(src: &str) -> Fixture {
        resolve_full(src).unwrap_or_else(|errors| {
            panic!(
                "expected resolution to succeed, got errors:\n{}",
                errors
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
    }

    #[test]
    fn registers_struct_field_and_function_and_param_defs() {
        let fx = resolve_ok("struct Foo { x: i32 } fn bar(a: i32) i32 { return a; }");

        let foo = fx.find_def("Foo").expect("struct Foo must be defined");
        assert!(matches!(foo.kind, DefKind::Struct));
        assert!(!foo.is_pub);

        let x = fx.find_def("x").expect("field x must be defined");
        assert!(matches!(x.kind, DefKind::Field));

        let bar = fx.find_def("bar").expect("function bar must be defined");
        assert!(matches!(bar.kind, DefKind::Function));

        let a = fx.find_def("a").expect("param a must be defined");
        assert!(matches!(a.kind, DefKind::Param));

        let foox_key = fx.find_def("x").expect("field x must be defined");
        assert!(foox_key.decl.is_some());
    }

    #[test]
    fn registers_struct_generic_param() {
        let fx = resolve_ok("struct Box[T] { value: T }");

        let t = fx.find_def("T").expect("generic T must be defined");
        assert!(matches!(t.kind, DefKind::GenericParam));
    }

    #[test]
    fn generic_implement_binding_slots_point_at_resolved_generic() {
        let fx = resolve_ok(
            "struct Box[T] { value: T } implement[U] Add: Box[U] { fn add(self) void {} }",
        );

        assert_eq!(fx.resolution.implement_generic_bindings.len(), 1);
        for resolution in fx.resolution.implement_generic_bindings.values() {
            assert!(matches!(resolution, Resolution::Def(_)));
        }
    }

    #[test]
    fn implement_names_tie_interface_and_object() {
        let fx = resolve_ok("struct Foo {} implement Foo : Copy {}");

        // The core library contributes its own implementations; only the
        // user's implementation is asserted here.
        let is_user_entry = |(iface, _): &(Resolution, Resolution)| match iface {
            Resolution::Def(def_id) => {
                fx.resolution
                    .defs
                    .get(def_id)
                    .map(|info| fx.name(info))
                    .as_deref()
                    == Some("Foo")
            }
            _ => false,
        };

        let entries: Vec<_> = fx
            .resolution
            .implement_names
            .values()
            .filter(|entry| is_user_entry(entry))
            .collect();
        assert_eq!(entries.len(), 1);
        assert!(matches!(entries[0].0, Resolution::Def(_)));
        assert!(matches!(entries[0].1, Resolution::Def(_)));
    }

    #[test]
    fn unresolved_type_error_is_reported() {
        let errs = resolve_full("struct Foo { x: Missing }").unwrap_err();

        assert_eq!(
            errs.iter()
                .filter(|e| matches!(e, ResolveError::UnresolvedType { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn unresolved_ident_error_is_reported() {
        let errs = resolve_full("fn main() { let a = b; }").unwrap_err();

        assert!(errs.iter().any(
            |e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name.as_str() == "b")
        ));
    }

    #[test]
    fn duplicate_type_definition_is_reported() {
        let errs = resolve_full("struct Foo {} struct Foo {}").unwrap_err();

        assert!(
            errs.iter()
                .any(|e| matches!(e, ResolveError::DuplicateDefinition { .. }))
        );
    }

    #[test]
    fn core_interface_name_is_reserved() {
        let errs = resolve_full("struct Display {}").unwrap_err();

        assert!(
            errs.iter()
                .any(|e| matches!(e, ResolveError::CoreReserved { .. }))
        );
    }

    #[test]
    fn registers_global_var_defs() {
        let fx = resolve_ok("let g: i32 = 0; pub const c: i32 = 1;");

        let g = fx.find_def("g").expect("global g must be defined");
        assert!(matches!(g.kind, DefKind::GlobalVar { is_const: false }));
        assert!(!g.is_pub);

        let c = fx.find_def("c").expect("global c must be defined");
        assert!(matches!(c.kind, DefKind::GlobalVar { is_const: true }));
        assert!(c.is_pub);
    }

    #[test]
    fn global_var_forward_reference_resolves() {
        resolve_ok("let a: i32 = b; let b: i32 = 0;");
    }

    #[test]
    fn global_var_visible_from_function() {
        resolve_ok("let g: i32 = 0; fn main() i32 { return g; }");
    }

    #[test]
    fn global_var_unresolved_ident_is_reported() {
        let errs = resolve_full("let a: i32 = missing;").unwrap_err();

        assert!(
            errs.iter()
                .any(|e| matches!(e, ResolveError::UnresolvedIdent { name, .. } if name.as_str() == "missing"))
        );
    }

    #[test]
    fn global_var_cycle_is_reported() {
        let errs = resolve_full("let a: i32 = b; let b: i32 = a;").unwrap_err();

        assert!(errs.iter().any(|e| matches!(
            e,
            ResolveError::GlobalVarCycle { chain, .. } if chain.as_str() == "a -> b -> a"
        )));
    }

    #[test]
    fn self_referencing_global_var_cycle_is_reported() {
        let errs = resolve_full("let a: i32 = a;").unwrap_err();

        assert!(
            errs.iter()
                .any(|e| matches!(e, ResolveError::GlobalVarCycle { chain, .. } if chain.as_str() == "a -> a"))
        );
    }

    #[test]
    fn global_var_dependencies_chain_is_ok() {
        resolve_ok("let a: i32 = 0; let b: i32 = a; let c: i32 = b;");
    }

    // --> Closures

    impl Fixture {
        fn def_id_by_name(&self, name: &str) -> Option<DefId> {
            self.resolution
                .defs
                .iter()
                .find(|(_, info)| self.name(info) == name)
                .map(|(id, _)| *id)
        }

        fn captured_names(&self, closure: DefId) -> Vec<String> {
            self.resolution.closure_captures[&closure]
                .iter()
                .map(|captured| {
                    let info = &self.resolution.defs[captured];
                    self.name(info)
                })
                .collect()
        }
    }

    #[test]
    fn closure_registers_def_and_captures_local() {
        let fx = resolve_ok("fn main() { let x = 1; let c = fn() i32 { return x; }; }");

        let closure = fx
            .def_id_by_name("closure0")
            .expect("closure0 def must be defined");
        let info = &fx.resolution.defs[&closure];
        assert!(matches!(info.kind, DefKind::Function));

        assert_eq!(fx.captured_names(closure), vec!["x".to_string()]);
    }

    #[test]
    fn zero_capture_closure_has_no_captures() {
        let fx = resolve_ok("fn main() { let x = 1; let c = fn() i32 { return 0; }; }");

        let closure = fx
            .def_id_by_name("closure0")
            .expect("closure0 def must be defined");

        assert!(!fx.resolution.closure_captures.contains_key(&closure));
    }

    #[test]
    fn nested_closure_capture_cascades_to_outer() {
        let fx = resolve_ok(
            "fn main() { let x = 1; let outer = fn() i32 { let y = 2; let inner = fn() i32 { return x + y; }; return inner(); }; }",
        );

        let outer = fx
            .def_id_by_name("closure0")
            .expect("closure0 (outer) def must be defined");
        let inner = fx
            .def_id_by_name("closure1")
            .expect("closure1 (inner) def must be defined");

        assert_eq!(fx.captured_names(outer), vec!["x".to_string()]);
        assert_eq!(
            fx.captured_names(inner),
            vec!["x".to_string(), "y".to_string()]
        );
    }

    #[test]
    fn closure_dedups_and_keeps_first_use_order() {
        let fx = resolve_ok(
            "fn main() { let a = 1; let b = 2; let c = fn() i32 { return a + b + a + a; }; }",
        );

        let closure = fx
            .def_id_by_name("closure0")
            .expect("closure0 def must be defined");

        assert_eq!(
            fx.captured_names(closure),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn closure_inside_nested_fn_captures_only_nested_fn_frame() {
        let fx = resolve_ok(
            "fn main() { let x = 1; fn nested() i32 { let y = 2; let c = fn() i32 { return y; }; return c(); } return x; }",
        );

        let closure = fx
            .def_id_by_name("closure0")
            .expect("closure0 def must be defined");

        // `x` lives in main's dead frame - only `y` (nested's frame) is captured
        assert_eq!(fx.captured_names(closure), vec!["y".to_string()]);
    }

    #[test]
    fn closure_in_nested_fn_cannot_reach_enclosing_frame() {
        let errs = resolve_full(
            "fn main() { let x = 1; fn nested() i32 { let c = fn() i32 { return x; }; return c(); } return 0; }",
        )
        .unwrap_err();

        assert!(errs.iter().any(
            |e| matches!(e, ResolveError::NestedFnCapture { name, .. } if name.as_str() == "x")
        ));
    }

    #[test]
    fn nested_fn_inside_closure_cannot_capture_closure_frame() {
        let errs = resolve_full(
            "fn main() { let x = 1; let c = fn() i32 { fn nested() i32 { return x; } return nested(); }; }",
        )
        .unwrap_err();

        assert!(errs.iter().any(
            |e| matches!(e, ResolveError::NestedFnCapture { name, .. } if name.as_str() == "x")
        ));
    }

    #[test]
    fn self_inside_closure_is_reported() {
        let errs = resolve_full(
            "interface Getter { fn get(self) i32; } struct Foo { v: i32 } implement Getter: Foo { fn get(self) i32 { let c = fn() i32 { return self.v; }; return c(); } }",
        )
        .unwrap_err();

        assert!(
            errs.iter()
                .any(|e| matches!(e, ResolveError::DisabledFeature { .. }))
        );
    }

    #[test]
    fn closure_param_named_self_is_reported() {
        let errs = resolve_full("fn main() { let c = fn(self) i32 { return 1; }; }").unwrap_err();

        assert!(
            errs.iter()
                .any(|e| matches!(e, ResolveError::DisabledFeature { .. }))
        );
    }

    #[test]
    fn closure_capturing_generic_param_is_reported() {
        let errs = resolve_full("fn generic[T](v: T) void { let c = fn(x: T) T { return x; }; }")
            .unwrap_err();

        assert!(
            errs.iter()
                .any(|e| matches!(e, ResolveError::DisabledFeature { .. }))
        );
    }

    #[test]
    fn closure_calling_sibling_function_is_not_a_capture() {
        let fx = resolve_ok(
            "fn helper() i32 { return 1; } fn main() { let c = fn() i32 { return helper(); }; }",
        );

        let closure = fx
            .def_id_by_name("closure0")
            .expect("closure0 def must be defined");

        assert!(!fx.resolution.closure_captures.contains_key(&closure));
    }
}
