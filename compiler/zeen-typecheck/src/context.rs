use std::{cell::RefCell, collections::HashMap, rc::Rc};

use zeen_resolve::{DefId, DefKind, ResolutionResult};
use zeen_types::TypeId;

#[derive(Debug)]
pub struct FnCtx {
    pub return_type: TypeId,
    pub self_type: Option<TypeId>,
    pub struct_def: Option<DefId>,
    pub generic_bindings: HashMap<DefId, TypeId>,
    pub generic_bounds: HashMap<DefId, Vec<DefId>>,
    pub loop_depth: u32,
}

#[derive(Default)]
pub struct TypeCheckCtx {
    stack: Vec<FnCtx>,
}

pub struct InterfaceRegistry {
    by_name: HashMap<String, DefId>,
}

impl TypeCheckCtx {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_fn(&mut self, ctx: FnCtx) {
        self.stack.push(ctx);
    }

    pub fn pop_fn(&mut self) {
        let _ = self.stack.pop();
    }

    pub fn current(&self) -> &FnCtx {
        self.stack
            .last()
            .expect("Current context called outside of any function body")
    }

    pub fn current_mut(&mut self) -> &mut FnCtx {
        self.stack
            .last_mut()
            .expect("Current context called outside of any function body")
    }

    pub fn in_loop(&self) -> bool {
        self.stack.last().is_some_and(|ctx| ctx.loop_depth > 0)
    }

    pub fn enter_loop(&mut self) {
        self.current_mut().loop_depth += 1;
    }

    pub fn exit_loop(&mut self) {
        self.current_mut().loop_depth -= 1;
    }

    pub fn generic_binding(&self, def_id: DefId) -> Option<TypeId> {
        self.stack.last()?.generic_bindings.get(&def_id).copied()
    }

    pub fn generic_bounds(&self, def_id: DefId) -> &[DefId] {
        self.stack
            .last()
            .and_then(|ctx| ctx.generic_bounds.get(&def_id))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

impl InterfaceRegistry {
    pub fn build(resolution: &ResolutionResult, rodeo: &Rc<RefCell<lasso::Rodeo>>) -> Self {
        let by_name = resolution
            .defs
            .iter()
            .filter(|(_, info)| matches!(info.kind, DefKind::Interface))
            .map(|(def_id, info)| (rodeo.borrow().resolve(&info.name).to_string(), *def_id))
            .collect();

        Self { by_name }
    }

    pub fn get(&self, name: &str) -> Option<DefId> {
        self.by_name.get(name).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

    use lasso::Rodeo;
    use miette::NamedSource;
    use zeen_ast::Source;
    use zeen_resolve::DefInfo;

    fn src(span: miette::SourceSpan) -> Source {
        Source {
            span,
            src: NamedSource::new("test.zn", Arc::new(String::new())),
        }
    }

    fn fn_ctx(return_type: TypeId) -> FnCtx {
        FnCtx {
            return_type,
            self_type: None,
            struct_def: None,
            generic_bindings: HashMap::new(),
            generic_bounds: HashMap::new(),
            loop_depth: 0,
        }
    }

    fn insert_def(
        resolution: &mut ResolutionResult,
        interner: &mut Rodeo,
        id: DefId,
        name: &str,
        kind: DefKind,
    ) {
        resolution.defs.insert(
            id,
            DefInfo {
                name: interner.get_or_intern(name),
                kind,
                span: src(0.into()),
                decl: None,
                is_pub: false,
            },
        );
    }

    #[test]
    #[should_panic(expected = "Current context called outside of any function body")]
    fn current_panics_outside_of_fn() {
        TypeCheckCtx::new().current();
    }

    #[test]
    #[should_panic(expected = "Current context called outside of any function body")]
    fn current_mut_panics_outside_of_fn() {
        TypeCheckCtx::new().current_mut();
    }

    #[test]
    fn push_and_pop_fn_round_trips() {
        let mut ctx = TypeCheckCtx::new();
        ctx.push_fn(fn_ctx(TypeId(1)));
        assert_eq!(ctx.current().return_type, TypeId(1));

        ctx.pop_fn();
        assert!(ctx.stack.is_empty());
    }

    #[test]
    fn current_returns_most_recent_fn() {
        let mut ctx = TypeCheckCtx::new();
        ctx.push_fn(fn_ctx(TypeId(1)));
        ctx.push_fn(fn_ctx(TypeId(2)));

        assert_eq!(ctx.current().return_type, TypeId(2));
    }

    #[test]
    fn loop_depth_tracks_enter_and_exit() {
        let mut ctx = TypeCheckCtx::new();
        ctx.push_fn(fn_ctx(TypeId(0)));
        assert!(!ctx.in_loop());

        ctx.enter_loop();
        assert!(ctx.in_loop());

        ctx.enter_loop();
        ctx.exit_loop();
        assert!(ctx.in_loop());

        ctx.exit_loop();
        assert!(!ctx.in_loop());
    }

    #[test]
    fn generic_binding_looks_up_inner_map() {
        let mut ctx = TypeCheckCtx::new();
        let mut f = fn_ctx(TypeId(0));
        f.generic_bindings.insert(DefId(1), TypeId(7));
        ctx.push_fn(f);

        assert_eq!(ctx.generic_binding(DefId(1)), Some(TypeId(7)));
        assert_eq!(ctx.generic_binding(DefId(2)), None);
        assert_eq!(TypeCheckCtx::new().generic_binding(DefId(1)), None);
    }

    #[test]
    fn generic_bounds_returns_empty_when_absent() {
        let mut ctx = TypeCheckCtx::new();
        ctx.push_fn(fn_ctx(TypeId(0)));

        assert!(ctx.generic_bounds(DefId(1)).is_empty());
        assert_eq!(TypeCheckCtx::new().generic_bounds(DefId(1)), &[]);
    }

    #[test]
    fn generic_bounds_returns_registered_bounds() {
        let mut ctx = TypeCheckCtx::new();
        let mut f = fn_ctx(TypeId(0));
        f.generic_bounds
            .insert(DefId(1), vec![DefId(10), DefId(11)]);
        ctx.push_fn(f);

        assert_eq!(ctx.generic_bounds(DefId(1)), &[DefId(10), DefId(11)]);
    }

    #[test]
    fn interface_registry_registers_only_interfaces() {
        let mut interner = Rodeo::default();
        let mut resolution = ResolutionResult::default();
        insert_def(
            &mut resolution,
            &mut interner,
            DefId(1),
            "Eq",
            DefKind::Interface,
        );
        insert_def(
            &mut resolution,
            &mut interner,
            DefId(2),
            "Vec3",
            DefKind::Struct,
        );

        let registry = InterfaceRegistry::build(&resolution, &Rc::new(RefCell::new(interner)));

        assert_eq!(registry.get("Eq"), Some(DefId(1)));
        assert_eq!(registry.get("Vec3"), None);
        assert_eq!(registry.get("missing"), None);
    }
}
