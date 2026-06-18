use zeen_ast::Source;
use zeen_resolve::DefId;

use lasso::Spur;
use miette::SourceSpan;
use std::rc::Rc;

use crate::{HirId, stmt::HirStmt, types::HirTypeExpr};

#[derive(Debug, Clone)]
pub struct HirExpr {
    pub id: HirId,
    pub kind: HirExprKind,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub enum HirExprKind {
    Literal(zeen_ast::expressions::Literal),

    VarRef(DefId),
    GenericParamRef(DefId),
    SelfValue(DefId),
    Macro(Spur),

    Binary {
        lhs: Rc<HirExpr>,
        rhs: Rc<HirExpr>,
        op: zeen_ast::expressions::BinaryOp,
    },

    Unary {
        expr: Rc<HirExpr>,
        op: zeen_ast::expressions::UnaryOp,
    },

    Call {
        callee: Rc<HirExpr>,
        args: Vec<Rc<HirExpr>>,
        generic_args: Vec<Rc<HirTypeExpr>>,
    },

    If {
        condition: Rc<HirExpr>,
        then_block: Rc<HirStmt>,
        else_block: Option<Rc<HirStmt>>,
    },

    Switch, // not implemented yet

    FieldAccess {
        object: Rc<HirExpr>,
        field: (Spur, SourceSpan),
    },

    SliceAccess {
        object: Rc<HirExpr>,
        index: Rc<HirExpr>,
    },

    StructInit {
        ty: (Option<DefId>, SourceSpan),
        fields: Vec<HirFieldInit>,
    },

    ArrayInit {
        elements: Vec<Rc<HirExpr>>,
    },

    Block(Vec<Rc<HirStmt>>),
    Type(Rc<HirTypeExpr>),

    Error,
}

#[derive(Debug, Clone)]
pub struct HirFieldInit {
    pub name: Spur,
    pub span: SourceSpan,
    pub value: Rc<HirExpr>,
}
