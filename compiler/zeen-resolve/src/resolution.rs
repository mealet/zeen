use lasso::Spur;
use std::collections::HashMap;

use zeen_ast::{
    Declaration, Expression, Source, Statement, TypeExpr,
    declarations::{EnumVariant, FnParam, GenericType, StructField},
    expressions::Arm,
};

/// A raw AST node pointer used as a map key.
/// SAFETY: only valid while the arena object is alive (the arena lives for
/// the whole program cycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeKey(pub usize);

impl NodeKey {
    pub fn from_expr(value: &Expression) -> Self {
        NodeKey(value as *const _ as usize)
    }

    pub fn from_type(value: &TypeExpr) -> Self {
        NodeKey(value as *const _ as usize)
    }

    pub fn from_stmt(value: &Statement) -> Self {
        NodeKey(value as *const _ as usize)
    }

    pub fn from_decl(value: &Declaration) -> Self {
        NodeKey(value as *const _ as usize)
    }

    pub fn from_param(p: &FnParam) -> Self {
        NodeKey(p as *const _ as usize)
    }

    pub fn from_field(f: &StructField) -> Self {
        NodeKey(f as *const _ as usize)
    }

    pub fn from_generic(g: &GenericType) -> Self {
        NodeKey(g as *const _ as usize)
    }

    pub fn from_variant(v: &EnumVariant) -> Self {
        NodeKey(v as *const _ as usize)
    }

    pub fn from_arm<'arena>(arm: &Arm<'arena>) -> Self {
        NodeKey(arm as *const _ as usize)
    }
}

/// `<decl pointer, slot index>` key for the generic bindings of an
/// `implement` declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BindingSlotKey(pub usize, pub usize);

/// Unique identifier for a resolver definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefId(pub u32);

/// Resolution instance that kept in symtable.
#[derive(Debug, Clone, Copy)]
pub enum Resolution {
    Def(DefId),          // function, struct, variable, etc.
    GenericParam(DefId), // scope generic param (functions/structures)
    SelfValue(DefId),    // `self` inside structures methods
    SelfType(DefId),     // `Self` alias
    Builtin,             // i32, u32, ...
    Error,
}

/// Keeping definition info here
#[derive(Debug, Clone)]
pub struct DefInfo {
    pub name: Spur,
    pub kind: DefKind,
    pub span: Source,
    pub decl: Option<NodeKey>, // may be useful
    pub is_pub: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DefKind {
    Function,
    Struct,
    Interface,
    InterfaceSelfPlaceholder,
    Enum,
    EnumVariant,
    TypeAlias,
    Variable { is_const: bool },
    Param,
    Field,
    GenericParam,
    ExternVar,
    GlobalVar { is_const: bool },
}

/// Final output of Name Resolver
#[derive(Debug, Clone, Default)]
pub struct ResolutionResult {
    /// Expr -> Resolution
    pub expr_bindings: HashMap<NodeKey, Resolution>,
    /// Type Expr -> Resolution
    pub type_bindings: HashMap<NodeKey, Resolution>,
    /// Generic bindings of `implement` decls -> Resolution (indexed by slot)
    pub implement_generic_bindings: HashMap<BindingSlotKey, Resolution>,
    /// All known defs
    pub defs: HashMap<DefId, DefInfo>,
    /// (struct, interface) -> methods
    pub impls: HashMap<(DefId, DefId), Vec<DefId>>,
    pub binding_sites: HashMap<NodeKey, DefId>,
    pub implement_names: HashMap<NodeKey, (Resolution, Resolution)>,
    pub interface_self_placeholders: HashMap<DefId, DefId>,

    /// Nested fn `DefId` -> enclosing fn `DefId`.
    pub nested_fn_parents: HashMap<DefId, DefId>,

    /// Closure `DefId` -> captured `DefId`s in first-use order.
    pub closure_captures: HashMap<DefId, Vec<DefId>>,

    /// Anonymous-struct-payload variant -> synthetic payload `DefKind::Struct`.
    pub enum_payload_struct_defs: HashMap<DefId, DefId>,
}

impl ResolutionResult {
    pub fn resolution_of_expr(&self, expr: &Expression) -> Option<Resolution> {
        self.expr_bindings.get(&NodeKey::from_expr(expr)).copied()
    }

    pub fn resolution_of_type(&self, texpr: &TypeExpr) -> Option<Resolution> {
        self.type_bindings
            .get(&NodeKey(texpr as *const _ as usize))
            .copied()
    }

    pub fn def_of_param(&self, p: &zeen_ast::declarations::FnParam) -> Option<DefId> {
        self.binding_sites.get(&NodeKey::from_param(p)).copied()
    }

    pub fn def_of_field(&self, f: &zeen_ast::declarations::StructField) -> Option<DefId> {
        self.binding_sites.get(&NodeKey::from_field(f)).copied()
    }

    pub fn def_of_generic(&self, g: &zeen_ast::declarations::GenericType) -> Option<DefId> {
        self.binding_sites.get(&NodeKey::from_generic(g)).copied()
    }

    pub fn def_of_variant(&self, v: &zeen_ast::declarations::EnumVariant) -> Option<DefId> {
        self.binding_sites.get(&NodeKey::from_variant(v)).copied()
    }

    pub fn def_of_arm<'arena>(&self, arm: &Arm<'arena>) -> Option<DefId> {
        self.binding_sites.get(&NodeKey::from_arm(arm)).copied()
    }

    pub fn def_of_enum_payload_struct(&self, variant: DefId) -> Option<DefId> {
        self.enum_payload_struct_defs.get(&variant).copied()
    }
}
