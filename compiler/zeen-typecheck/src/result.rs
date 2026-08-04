use std::collections::HashMap;

use zeen_hir::HirId;
use zeen_resolve::DefId;
use zeen_types::{StructTypeInfo, TypeId, TypeInterner};

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
    pub method_owner: HashMap<DefId, DefId>,
    pub const_bindings: HashMap<DefId, bool>,
    pub format_specs: HashMap<HirId, Vec<FormatChunk>>,
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
