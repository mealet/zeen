use zeen_ast::Source;
use zeen_resolve::DefId;

use lasso::Spur;
use miette::SourceSpan;
use std::rc::Rc;

use crate::{HirId, stmt::HirStmt, types::HirTypeExpr};

#[derive(Debug, Clone)]
pub struct HirDecl {
    pub id: HirId,
    pub def_id: DefId,
    pub kind: HirDeclKind,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub enum HirDeclKind {
    Fn(Rc<HirFn>),
    Struct(Rc<HirStruct>),
    Interface(Rc<HirInterface>),
    Implement(Rc<HirImplement>),
    Enum(Rc<HirEnum>),

    Alias(Rc<HirAlias>),

    ExternVar {
        name: (Spur, SourceSpan),
        ty: Rc<HirTypeExpr>,
        is_pub: bool,
    },

    GlobalVar {
        name: (Spur, SourceSpan),
        ty: Rc<HirTypeExpr>,
        value: Rc<crate::expr::HirExpr>,
        is_const: bool,
        is_pub: bool,
    },

    // resolved at `zeen-resolve` stage
    ExternLink,
    ExternInclude,
}

// ==| Decls Structures |==

// -> Fn

#[derive(Debug, Clone)]
pub struct HirFn {
    pub name: (Spur, SourceSpan),

    pub generics: Vec<HirGenericParam>,
    pub params: Vec<Rc<HirParam>>,
    pub return_type: Option<Rc<HirTypeExpr>>,
    pub where_clauses: Vec<HirWhereClause>,
    pub body: Option<Rc<HirStmt>>,

    pub is_pub: bool,
    pub is_extern: bool,

    pub self_param: Option<DefId>,
    pub parent_fn: Option<DefId>,
}

#[derive(Debug, Clone)]
pub struct HirWhereClause {
    pub ty: Rc<HirTypeExpr>,
    pub kind: HirWhereClauseKind,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub enum HirWhereClauseKind {
    Bound { bounds: Vec<DefId> },
    Equality { ty: Rc<HirTypeExpr> },
}

#[derive(Debug, Clone)]
pub struct HirParam {
    pub id: HirId,
    pub def_id: Option<DefId>,
    pub name: Option<Spur>,
    pub ty: Rc<HirTypeExpr>,
    pub span: SourceSpan,
}

#[derive(Debug, Clone)]
pub struct HirGenericParam {
    pub def_id: DefId,
    pub name: (Spur, SourceSpan),
    pub bounds: Vec<DefId>,
}

// -> Struct

#[derive(Debug, Clone)]
pub struct HirStruct {
    pub name: (Spur, SourceSpan),
    pub is_pub: bool,

    pub generics: Vec<HirGenericParam>,
    pub fields: Vec<HirField>,
    pub methods: Vec<Rc<HirDecl>>, // HirDeclKind::Fn
}

#[derive(Debug, Clone)]
pub struct HirField {
    pub def_id: DefId,
    pub name: Spur,
    pub ty: Rc<HirTypeExpr>,
    pub is_pub: bool,
}

// -> Interface

#[derive(Debug, Clone)]
pub struct HirInterface {
    pub name: (Spur, SourceSpan),
    pub is_pub: bool,

    pub generics: Vec<HirGenericParam>,
    pub aliases: Vec<Rc<HirAssociatedType>>,
    pub methods: Vec<Rc<HirDecl>>, // HirDeclKind::Fn
}

#[derive(Debug, Clone)]
pub struct HirAssociatedType {
    pub def_id: DefId,
    pub name: (Spur, SourceSpan),
    pub bounds: Vec<DefId>,
    pub default: Option<Rc<HirTypeExpr>>,
}

// -> Implement

#[derive(Debug, Clone)]
pub struct HirImplement {
    pub interface: Option<DefId>,
    pub interface_args: Vec<Rc<HirTypeExpr>>,
    pub object: Option<DefId>,

    pub generics: Vec<HirGenericParam>,
    pub where_clauses: Vec<HirWhereClause>,
    pub object_generics_bindings: Vec<DefId>,
    pub object_generic_types: Vec<Rc<HirTypeExpr>>,
    pub object_bindings_span: SourceSpan,

    pub aliases: Vec<Rc<HirAssociatedType>>,
    pub methods: Vec<Rc<HirDecl>>, // HirDeclKind::Fn
}

// -> Enum

#[derive(Debug, Clone)]
pub struct HirEnum {
    pub name: (Spur, SourceSpan),
    pub is_pub: bool,

    pub generics: Vec<HirGenericParam>,
    pub variants: Vec<HirEnumVariant>,
    pub methods: Vec<Rc<HirDecl>>, // HirDeclKind::Fn
}

#[derive(Debug, Clone)]
pub struct HirEnumVariant {
    pub def_id: DefId,
    pub name: Spur,
    pub span: SourceSpan,
    pub payload: Option<HirEnumVariantPayload>,
}

#[derive(Debug, Clone)]
pub enum HirEnumVariantPayload {
    /// `b: i32` - a single typed value
    Single(Rc<HirTypeExpr>),
    /// `c: { fields }` - an anonymous struct payload
    Anonymous {
        def_id: DefId,
        fields: Vec<HirField>,
    },
}

// -> Alias

#[derive(Debug, Clone)]
pub struct HirAlias {
    pub name: (Spur, SourceSpan),
    pub is_pub: bool,
    pub generics: Vec<HirGenericParam>,
    pub ty: Rc<HirTypeExpr>,
}
