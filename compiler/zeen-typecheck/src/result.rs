use std::collections::HashMap;

use zeen_ast::Source;
use zeen_hir::HirId;
use zeen_resolve::DefId;
use zeen_types::{StructTypeInfo, TypeId, TypeInterner};

use crate::closure_alloc::ClosureAllocKind;
use crate::format_str::FormatChunk;

#[derive(Debug, Default)]
pub struct TypeCheckResult {
    pub main_fn_def: Option<DefId>,
    pub interner: TypeInterner,
    pub expr_types: HashMap<HirId, TypeId>,
    pub def_types: HashMap<DefId, TypeId>,
    pub call_resolutions: HashMap<HirId, CallResolution>,
    pub field_resolutions: HashMap<HirId, DefId>,
    pub operator_resolutions: HashMap<HirId, OperatorResolution>,
    pub struct_info: HashMap<DefId, StructTypeInfo>,
    pub struct_generics: HashMap<DefId, Vec<DefId>>,
    pub enum_variants: HashMap<DefId, Vec<DefId>>,
    pub method_owner: HashMap<DefId, DefId>,
    pub const_bindings: HashMap<DefId, bool>,
    pub format_specs: HashMap<HirId, Vec<FormatChunk>>,
    /// Per-closure-site environment allocation decision (key = the closure's
    /// synthetic fn `DefId`), see `closure_alloc`.
    pub closure_allocs: HashMap<DefId, ClosureAllocKind>,

    /// Return expressions of functions whose declared return is a `Fn`/
    /// `FnOnce` bound. The concrete closure type of the return is derived
    /// from these after all bodies have been checked.
    pub fat_return_candidates: HashMap<DefId, Vec<(HirId, Source)>>,

    /// Variable defs whose initializer is a fat (or fat-bound-typed) value;
    /// used to resolve the erased bound annotations down to concrete closure
    /// storage types.
    pub fat_let_values: HashMap<DefId, HirId>,

    /// VarRef expressions whose recorded type is a fat value; used by the
    /// finalization to resolve erased bounds through variables.
    pub fat_value_defs: HashMap<HirId, DefId>,

    /// Resolved concrete return type of a function declared to return a
    /// `Fn`/`FnOnce` bound.
    pub fn_return_fats: HashMap<DefId, TypeId>,
}

impl TypeCheckResult {
    pub fn record_expr_type(&mut self, id: HirId, ty: TypeId) {
        self.expr_types.insert(id, ty);
    }
}

#[derive(Debug)]
pub struct CallResolution {
    pub fn_def: DefId,
    pub generic_args: Vec<TypeId>,
}

#[derive(Debug, Clone)]
pub struct OperatorResolution {
    pub method_def: DefId,
    pub generic_args: Vec<TypeId>,
}
