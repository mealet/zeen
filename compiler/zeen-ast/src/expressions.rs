use lasso::Spur;
use miette::SourceSpan;

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Expression<'arena> {
    pub kind: ExpressionKind<'arena>,
    pub span: SourceSpan,
}

impl Expression<'_> {
    pub fn merge_span(&self, other: SourceSpan) -> SourceSpan {
        let start = self.span.offset().min(other.offset());
        let end = (self.span.offset() + self.span.len()).max(other.offset() + other.len());

        SourceSpan::new(start.into(), end - start)
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ExpressionKind<'arena> {
    Literal(Literal),

    Ident {
        name: Spur,
        generic_args: Option<&'arena [&'arena crate::types::TypeExpr<'arena>]>,
    },

    Macro(Spur),

    Binary {
        lhs: &'arena Expression<'arena>,
        rhs: &'arena Expression<'arena>,
        op: BinaryOp,
    },

    Unary {
        expr: &'arena Expression<'arena>,
        op: UnaryOp,
    },

    Call {
        callee: &'arena Expression<'arena>,
        args: &'arena [&'arena Expression<'arena>],
    },

    If {
        condition: &'arena Expression<'arena>,
        then_block: &'arena crate::statements::Statement<'arena>,
        else_block: Option<&'arena crate::statements::Statement<'arena>>,
    },

    Switch {
        object: &'arena Expression<'arena>,
        arms: &'arena [&'arena Arm<'arena>],
    },

    FieldAccess {
        object: &'arena Expression<'arena>,
        field: &'arena Expression<'arena>, // ident expr
    },

    SliceAccess {
        object: &'arena Expression<'arena>,
        index: &'arena Expression<'arena>,
    },

    StructInit {
        ty: &'arena Expression<'arena>,
        fields: Option<&'arena [FieldInit<'arena>]>,
    },

    ArrayInit {
        elements: &'arena [&'arena Expression<'arena>],
    },

    Block(&'arena [&'arena crate::statements::Statement<'arena>]),
    Type(&'arena crate::types::TypeExpr<'arena>),
}

// Literal

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Literal {
    Int(i64),
    Float(f64),
    Char(char),
    ByteChar(char),
    Bool(bool),
    String(Spur),
    Null,
}

// Binary

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum BinaryOp {
    // Arithmetic
    Add, // +
    Sub, // -
    Mul, // *
    Div, // /
    Mod, // %

    // Comparison
    Eq, // ==
    Ne, // !=
    Lt, // <
    Gt, // >
    Le, // <= / =<
    Ge, // >= / =>

    // Boolean
    LogicalAnd, // &&
    LogicalOr,  // ||

    // Bitwise
    BitAnd, // &
    BitOr,  // |
    BitXor, // ^
    Shl,    // <<
    Shr,    // >>
}

// Unary

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum UnaryOp {
    Neg,    // -a
    Not,    // !a (boolean)
    BitNot, // ~
    Deref,  // *a
    AddrOf, // &a
}

// Struct Init

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct FieldInit<'arena> {
    pub name: Spur,
    pub value: &'arena Expression<'arena>,
    pub span: SourceSpan,
}

// Switch

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Arm<'arena> {
    pub pattern: Pattern<'arena>,
    pub body: &'arena Expression<'arena>,
    pub guard: Option<&'arena Expression<'arena>>,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Pattern<'arena> {
    Literal(Literal),
    Named(Spur),
    Or(&'arena [Pattern<'arena>]),
    Wildcard,
}
