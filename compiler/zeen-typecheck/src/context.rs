use std::collections::HashMap;

use crate::types::TypeId;
use zeen_resolve::DefId;

#[derive(Debug)]
pub struct FnCtx {
    pub return_type: TypeId,
    pub self_type: Option<TypeId>,
    pub generic_bindings: HashMap<DefId, TypeId>,
    pub generic_bounds: HashMap<DefId, Vec<DefId>>,
    pub loop_depth: u32,
}

pub struct TypeCheckCtx {
    stack: Vec<FnCtx>,
}

impl TypeCheckCtx {
    pub fn new() -> Self {
        Self { stack: Vec::new() }
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

    pub fn bind_generic(&mut self, def_id: DefId, ty: TypeId) {
        self.current_mut().generic_bindings.insert(def_id, ty);
    }

    pub fn generic_bounds(&self, def_id: DefId) -> &[DefId] {
        self.stack
            .last()
            .and_then(|ctx| ctx.generic_bounds.get(&def_id))
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}
