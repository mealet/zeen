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
        args: &'arena [Expression<'arena>],
        generic_args: Option<&'arena [crate::types::TypeExpr<'arena>]>,
    },

    If {
        condition: &'arena Expression<'arena>,
        then_block: &'arena Expression<'arena>,
        else_block: Option<&'arena Expression<'arena>>,
    },

    Switch {
        object: &'arena Expression<'arena>,
        arms: &'arena [Arm<'arena>],
    },

    FieldAccess {
        object: &'arena Expression<'arena>,
        field: Spur,
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
        elements: &'arena [Expression<'arena>],
    },

    Block(&'arena [crate::statements::Statement<'arena>]),
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
    name: Spur,
    value: &'arena Expression<'arena>,
}

// Switch

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Arm<'arena> {
    pattern: Pattern<'arena>,
    body: &'arena Expression<'arena>,
    guard: Option<&'arena Expression<'arena>>,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Pattern<'arena> {
    Literal(Literal),
    Named(Spur),
    Or(&'arena [Pattern<'arena>]),
    Wildcard,
}
