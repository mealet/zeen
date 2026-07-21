use std::collections::HashMap;

use zeen_hir::HirId;
use zeen_resolve::DefId;
use zeen_types::{StructTypeInfo, TypeId, TypeInterner};

use crate::{error::TypeError, format_str::FormatChunk};

#[derive(Debug, Default)]
pub struct TypeCheckResult {
    pub interner: TypeInterner,
    pub expr_types: HashMap<HirId, TypeId>,
    pub def_types: HashMap<DefId, TypeId>,
    pub call_resolutions: HashMap<HirId, CallResolution>,
    pub field_resolutions: HashMap<HirId, DefId>,
    pub struct_info: HashMap<DefId, StructTypeInfo>,
    pub const_bindings: HashMap<DefId, bool>,
    pub format_specs: HashMap<HirId, Vec<FormatChunk>>,
    pub errors: Vec<TypeError>,
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
