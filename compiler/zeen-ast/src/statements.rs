use lasso::Spur;
use miette::SourceSpan;

use crate::{
    expressions::{self, Expression},
    types::TypeExpr,
};

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Statement<'arena> {
    pub kind: StatementKind<'arena>,
    pub span: SourceSpan,
}

impl Statement<'_> {
    pub fn merge_span(&self, other: SourceSpan) -> SourceSpan {
        let start = self.span.offset().min(other.offset());
        let end = (self.span.offset() + self.span.len()).max(other.offset() + other.len());

        SourceSpan::new(start.into(), end - start)
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum StatementKind<'arena> {
    Let {
        name: Spur,
        explicit_type: Option<&'arena TypeExpr<'arena>>,
        value: Option<&'arena Expression<'arena>>,
        is_const: bool,
    },

    Assign {
        object: &'arena Expression<'arena>,
        value: &'arena Expression<'arena>,
    },

    CompoundAssign {
        object: &'arena Expression<'arena>,
        value: &'arena Expression<'arena>,
        op: expressions::BinaryOp,
    },

    Return {
        value: Option<&'arena Expression<'arena>>,
    },

    Break,
    Continue,

    While {
        condition: &'arena Expression<'arena>,
        block: &'arena Statement<'arena>,
    },

    For {
        varname: (Spur, SourceSpan),
        iterator: &'arena Expression<'arena>,
        block: &'arena Statement<'arena>,
    },

    Expr(&'arena Expression<'arena>),
    TrailingExpr(&'arena Expression<'arena>), // tech node, converts to Expr
}
