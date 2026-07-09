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

    MacroCall {
        kind: (HirMacroKind, SourceSpan),
        args: Vec<Rc<HirExpr>>,
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

    Block {
        stmts: Vec<Rc<HirStmt>>,
        trailing: Option<Rc<HirExpr>>,
    },
    Type(Rc<HirTypeExpr>),

    Error,
}

#[derive(Debug, Clone)]
pub struct HirFieldInit {
    pub name: Spur,
    pub span: SourceSpan,
    pub value: Rc<HirExpr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirMacroKind {
    As,      // @as(T, expr) -> T
    SizeOf,  // @sizeof(T) -> usize
    AlignOf, // @alignof(T) -> usize

    Print,   // @print("format", ...) -> void
    Println, // @println("format", ...) -> void
    Format,  // @format("format", ...) -> *char

    Panic,       // @panic("format", ...) -> never
    Unreachable, // @unreachable() -> never
    Dbg,         // @dbg(expr) -> expr

    Unknown, // Unknown macro fallback
}
