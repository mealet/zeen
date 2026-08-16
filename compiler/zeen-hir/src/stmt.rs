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

    Break,
    Continue,

    While {
        condition: Rc<HirExpr>,
        block: Rc<HirStmt>,
    },

    For {
        def_id: DefId,
        varname: (Spur, SourceSpan),
        iterator: Rc<HirExpr>,
        block: Rc<HirStmt>,
    },

    Expr(Rc<HirExpr>),

    /// Nested function declaration. The function itself is collected and
    /// lowered like any other, but it is only nameable from its parent.
    FnDecl(Rc<HirDecl>),

    Error,
}
