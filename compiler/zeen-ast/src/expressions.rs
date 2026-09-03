use lasso::Spur;
use miette::SourceSpan;

use crate::declarations::{DirectiveValue, PreprocessorDirective};

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

    MacroCall {
        name: (Spur, SourceSpan),
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

    ArrayRepeatInit {
        element: &'arena Expression<'arena>,
        len: &'arena Expression<'arena>,
    },

    Block {
        stmts: &'arena [&'arena crate::statements::Statement<'arena>],
        trailing: Option<&'arena Expression<'arena>>,
    },

    /// Anonymous function expression: `fn(params) ret { body }`. Lowered to a
    /// synthetic function capturing its environment by value.
    Closure {
        params: &'arena [crate::declarations::FnParam<'arena>],
        return_type: Option<&'arena crate::types::TypeExpr<'arena>>,
        body: &'arena crate::statements::Statement<'arena>,
    },

    Type(&'arena crate::types::TypeExpr<'arena>),

    /// Compile-time target constant (`@var[os]`), resolved by the preprocessor.
    TargetVar(TargetVarKind),

    /// An expression guarded by a target condition (`@os[linux] { expr } else { expr }`).
    /// Resolved by the preprocessor: the whole expression is replaced by the
    /// body of the single matching branch.
    ConditionalBlock(&'arena ExprConditionalBlock<'arena>),
}

/// A `@name[values] { expr }` guard at expression level with an optional `else`.
/// `else_block` is either another expression conditional (else-if) or a bare-else
/// expression holding the fallback body.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct ExprConditionalBlock<'arena> {
    pub directive: PreprocessorDirective,
    pub values: &'arena [DirectiveValue<'arena>],
    pub body: &'arena Expression<'arena>,
    pub bare_else: bool,
    pub else_block: Option<&'arena Expression<'arena>>,
}

/// Target information provided at compile time via `@var[name]`.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum TargetVarKind {
    Os,
    Arch,
    Env,
    Target,
    Family,
    Debug,
    Release,
}

impl TargetVarKind {
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "os" => Some(Self::Os),
            "arch" => Some(Self::Arch),
            "env" => Some(Self::Env),
            "target" => Some(Self::Target),
            "family" => Some(Self::Family),
            "debug" => Some(Self::Debug),
            "release" => Some(Self::Release),
            _ => None,
        }
    }
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
    Ge, // >=

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

impl std::fmt::Display for BinaryOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Add => "+",
                Self::Sub => "-",
                Self::Mul => "*",
                Self::Div => "/",
                Self::Mod => "%",

                Self::Eq => "==",
                Self::Ne => "!=",
                Self::Lt => "<",
                Self::Gt => ">",
                Self::Le => "<=",
                Self::Ge => ">=",

                Self::LogicalAnd => "&&",
                Self::LogicalOr => "||",

                Self::BitAnd => "&",
                Self::BitOr => "|",
                Self::BitXor => "^",
                Self::Shl => "<<",
                Self::Shr => ">>",
            }
        )
    }
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

impl std::fmt::Display for UnaryOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Neg => "-",
                Self::Not => "!",
                Self::BitNot => "~",
                Self::Deref => "*",
                Self::AddrOf => "&",
            }
        )
    }
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
