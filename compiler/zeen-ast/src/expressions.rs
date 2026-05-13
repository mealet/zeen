use lasso::Spur;
use miette::SourceSpan;

// NOTE: Spur is a key for `lasso` string interner.

pub struct Expression<'arena> {
    kind: ExpressionKind<'arena>,
    span: SourceSpan,
}

pub enum ExpressionKind<'arena> {
    Literal(ExpressionLiteral),
    Ident(Spur),
    Macro(Spur),

    Binary {
        lhs: &'arena ExpressionKind<'arena>,
        rhs: &'arena ExpressionKind<'arena>,
        op: BinaryOp,
    },

    Unary {
        expr: &'arena ExpressionKind<'arena>,
        op: UnaryOp,
    },
}

// Literal

pub enum ExpressionLiteral {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(Spur),
    Null,
}

// Binary

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
    Logicaland, // &&
    LogicalOr,  // ||

    // Bitwise
    BitAnd, // &
    BitOr,  // |
    BitXor, // ^
    Shl,    // <<
    Shr,    // >>
}

// Unary

pub enum UnaryOp {
    Neg,    // -a
    Not,    // !a (boolean)
    BitNot, // ~
    Deref,  // *a
    AddrOf, // &a
}
