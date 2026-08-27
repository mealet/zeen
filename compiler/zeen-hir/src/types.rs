use zeen_ast::Source;
use zeen_resolve::DefId;

use std::rc::Rc;

use crate::{HirId, decl::HirGenericParam, expr::HirExpr};

#[derive(Debug)]
pub struct HirTypeExpr {
    pub id: HirId,
    pub kind: HirTypeKind,
    pub source: Source,
}

#[derive(Debug)]
pub enum HirTypeKind {
    Builtin(zeen_ast::types::BuiltinType),

    SelfType(DefId),
    SelfAlias(DefId),
    VaArgs,

    Named {
        def_id: DefId,
        generic_args: Vec<Rc<HirTypeExpr>>,
    },

    Const(Rc<HirTypeExpr>),

    /// `typeof <expr>` - type inferred from the expression's type, without
    /// evaluating it.
    TypeOf(Rc<HirExpr>),

    SinglePointer(Rc<HirTypeExpr>),
    ManyPointer(Rc<HirTypeExpr>),

    Array {
        element: Rc<HirTypeExpr>,
        len: Option<Rc<HirExpr>>,
    },

    Fn {
        params: Vec<Rc<HirTypeExpr>>,
        generics: Vec<HirGenericParam>,
        ret: Rc<HirTypeExpr>,
    },

    Error,
}
