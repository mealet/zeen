use zeen_ast::Source;
use zeen_resolve::DefId;

use std::rc::Rc;
use miette::SourceSpan;
use lasso::Spur;

use crate::{
    HirId,
    stmt::HirStmt,
    types::HirTypeExpr,
};

/// Declaration in HIR (High Level Representation) version
#[derive(Debug, Clone)]
pub struct HirDecl {
    pub id: HirId,
    pub def_id: DefId,
    pub kind: HirDeclKind,

    /// Source contains span of Declaration and ref to the current module source code
    pub source: Source,
}

#[derive(Debug, Clone)]
pub enum HirDeclKind {
    Fn(Rc<HirFn>),
    Struct(Rc<HirStruct>),
    Interface(Rc<HirInterface>),
    Implement(Rc<HirImplement>),
}

// ==| Decls Structures |==

// -> Fn

#[derive(Debug, Clone)]
pub struct HirFn {
    pub name: (Spur, SourceSpan),

    pub generics: usize,
    pub params: Vec<Rc<HirParam>>,
    pub return_type: Option<Rc<HirTypeExpr>>,

    pub body: Option<Rc<HirStmt>>,

    pub is_pub: bool,
    pub is_extern: bool,

    pub self_param: Option<DefId>,
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
    pub name: Spur,
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
    pub methods: Vec<Rc<HirDecl>>, // HirDeclKind::Fn
}

// -> Implement

#[derive(Debug, Clone)]
pub struct HirImplement {
    pub interface: Option<DefId>,
    pub object: Option<DefId>,

    pub methods: Vec<Rc<HirDecl>>, // HirDeclKind::Fn
}
