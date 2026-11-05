use zeen_ast::Source;
use zeen_resolve::DefId;

use lasso::Spur;
use miette::SourceSpan;
use std::rc::Rc;

use crate::{HirId, decl::HirDecl, expr::HirExpr, types::HirTypeExpr};

#[derive(Debug, Clone)]
pub struct HirStmt {
    pub id: HirId,
    pub kind: HirStmtKind,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub enum HirStmtKind {
    Let {
        def_id: DefId,
        name: Spur,
        explicit_type: Option<Rc<HirTypeExpr>>,
        value: Option<Rc<HirExpr>>,
        is_const: bool,
    },

    Assign {
        object: Rc<HirExpr>,
        value: Rc<HirExpr>,
    },

    CompoundAssign {
        object: Rc<HirExpr>,
        value: Rc<HirExpr>,
        op: zeen_ast::expressions::BinaryOp,
    },

    Return {
        value: Option<Rc<HirExpr>>,
    },

    Break {
        label: Option<Spur>,
    },

    Continue {
        label: Option<Spur>,
    },

    While {
        label: Option<Spur>,
        condition: Rc<HirExpr>,
        update: Option<Rc<HirStmt>>,
        block: Rc<HirStmt>,
        else_block: Option<Rc<HirStmt>>,
    },

    For {
        label: Option<Spur>,
        def_id: DefId,
        varname: (Spur, SourceSpan),
        iterator: Rc<HirExpr>,
        block: Rc<HirStmt>,
        else_block: Option<Rc<HirStmt>>,
    },

    Expr(Rc<HirExpr>),

    FnDecl(Rc<HirDecl>),

    Error,
}
