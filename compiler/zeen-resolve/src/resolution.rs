use lasso::Spur;
use miette::SourceSpan;
use std::collections::HashMap;

use zeen_ast::{
    Declaration, Expression, Source, Statement, TypeExpr,
    declarations::{EnumVariant, FnParam, GenericType, StructField},
};

/// A simple representation of allocated AST node as a key.
/// SAFETY: This thing is very dangerous, can be used when you sure your object is live as long as
/// NodeKey does. In compiler it is used with arena (lives whole program cycle) allocated objects.
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

    pub fn from_binding_slot(decl: &Declaration, index: usize) -> Self {
        let base = decl as *const _ as usize;
        NodeKey(base.wrapping_add(index + 1))
    }
}

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
}

#[derive(Debug, Clone)]
pub enum DefKind {
    Function,
    Struct,
    Interface,
    InterfaceSelfPlaceholder,
    Enum,
    EnumVariant,
    Variable { is_const: bool },
    Param,
    Field,
    GenericParam,
    ExternVar,
}

/// Final output of Name Resolver
#[derive(Debug, Clone, Default)]
pub struct ResolutionResult {
    /// Expr -> Resolution
    pub expr_bindings: HashMap<NodeKey, Resolution>,
    /// Type Expr -> Resolution
    pub type_bindings: HashMap<NodeKey, Resolution>,
    /// All known defs
    pub defs: HashMap<DefId, DefInfo>,
    /// (struct, interface) -> methods
    pub impls: HashMap<(DefId, DefId), Vec<DefId>>,
    pub binding_sites: HashMap<NodeKey, DefId>,
    pub implement_names: HashMap<NodeKey, (Resolution, Resolution)>,
    pub interface_self_placeholders: HashMap<DefId, DefId>,
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
}
