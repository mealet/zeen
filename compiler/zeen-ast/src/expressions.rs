use lasso::Spur;
use miette::SourceSpan;

pub struct Expression<'arena> {
    kind: ExpressionKind<'arena>,
    span: SourceSpan,
}

pub enum ExpressionKind<'arena> {
    Literal(ExpressionLiteral),
    Ident(Spur),
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

    Block(&'arena [crate::statements::Statements<'arena>]),
    Type(&'arena crate::types::TypeExpr<'arena>),
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

// Struct Init

pub struct FieldInit<'arena> {
    name: Spur,
    value: &'arena Expression<'arena>,
}

// Switch

pub struct Arm<'arena> {
    pattern: Pattern<'arena>,
    body: &'arena Expression<'arena>,
    guard: Option<&'arena Expression<'arena>>,
}

pub enum Pattern<'arena> {
    Literal(ExpressionLiteral),
    Named(Spur),
    Or(&'arena [Pattern<'arena>]),
    Wildcard,
}
