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

    let mut include_resolver = include_resolver::IncludeResolver::new(
        Rc::clone(&filename),
        Arc::clone(&src),
        arena,
        Rc::clone(&interner),
        context,
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
            core_files: vec![("core.ops", CORE_OPS)],
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

        let entries: Vec<_> = fx.resolution.implement_names.values().collect();
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
}
