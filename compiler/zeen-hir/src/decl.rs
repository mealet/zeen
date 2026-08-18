use zeen_ast::Source;
use zeen_resolve::DefId;

use lasso::Spur;
use miette::SourceSpan;
use std::rc::Rc;

use crate::{HirId, stmt::HirStmt, types::HirTypeExpr};

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
    Enum(Rc<HirEnum>),

    ExternVar {
        name: (Spur, SourceSpan),
        ty: Rc<HirTypeExpr>,
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

    pub body: Option<Rc<HirStmt>>,

    pub is_pub: bool,
    pub is_extern: bool,

    pub self_param: Option<DefId>,

    /// For nested functions: `DefId` of the enclosing function. Used to build
    /// `<parent>-><name>` MIR/LLVM symbols.
    pub parent_fn: Option<DefId>,
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
    pub methods: Vec<Rc<HirDecl>>, // HirDeclKind::Fn
}

// -> Implement

#[derive(Debug, Clone)]
pub struct HirImplement {
    pub interface: Option<DefId>,
    pub object: Option<DefId>,

    pub generics: Vec<HirGenericParam>,
    pub object_generics_bindings: Vec<DefId>,
    pub object_bindings_span: SourceSpan,

    pub methods: Vec<Rc<HirDecl>>, // HirDeclKind::Fn
}

// -> Enum

#[derive(Debug, Clone)]
pub struct HirEnum {
    pub name: (Spur, SourceSpan),
    pub is_pub: bool,
    pub variants: Vec<HirEnumVariant>,
}

#[derive(Debug, Clone)]
pub struct HirEnumVariant {
    pub def_id: DefId,
    pub name: Spur,
    pub span: SourceSpan,
}
