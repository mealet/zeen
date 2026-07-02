/// Compiler reserved interfaces names.
#[derive(Debug, Clone, Copy, PartialEq, Hash, Eq)]
pub enum WellKnownInterfaces {
    Display,
    Debug,

    Copy,
    Drop,

    Eq,

    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,

    Neg,
    Not,
    BitNot,

    Deref,
    DerefPtr,
    Slice,
    SlicePtr,
}

impl WellKnownInterfaces {
    pub const ALL: &'static [WellKnownInterfaces] = &[
        Self::Display,
        Self::Debug,
        Self::Copy,
        Self::Drop,
        Self::Eq,
        Self::Add,
        Self::Sub,
        Self::Mul,
        Self::Div,
        Self::Mod,
        Self::BitAnd,
        Self::BitOr,
        Self::BitXor,
        Self::Shl,
        Self::Shr,
        Self::Neg,
        Self::Not,
        Self::BitNot,
        Self::Deref,
        Self::DerefPtr,
        Self::Slice,
        Self::SlicePtr,
    ];

    pub fn name(self) -> String {
        format!("{:?}", self)
    }

    pub fn method_name(self) -> &'static str {
        match self {
            Self::Display => "display",
            Self::Debug => "debug",

            Self::Copy => "copy",
            Self::Drop => "drop",

            Self::Eq => "eq",

            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
            Self::Div => "div",
            Self::Mod => "mod",
            Self::BitAnd => "bit_and",
            Self::BitOr => "bit_or",
            Self::BitXor => "bit_xor",
            Self::Shl => "shl",
            Self::Shr => "shr",

            Self::Neg => "neg",
            Self::Not => "not",
            Self::BitNot => "bit_not",

            Self::Deref => "deref",
            Self::DerefPtr => "deref_ptr",
            Self::Slice => "slice",
            Self::SlicePtr => "slice_ptr",
        }
    }

    pub fn shape(self) -> MethodShape {
        match self {
            Self::Display
            | Self::Debug
            | Self::Drop
            | Self::Copy
            | Self::Neg
            | Self::Not
            | Self::BitNot
            | Self::Deref
            | Self::DerefPtr => MethodShape::Unary,

            Self::Eq
            | Self::Add
            | Self::Sub
            | Self::Mul
            | Self::Div
            | Self::Mod
            | Self::BitAnd
            | Self::BitOr
            | Self::BitXor
            | Self::Shl
            | Self::Shr => MethodShape::Binary,

            Self::Slice | Self::SlicePtr => MethodShape::Indexed,
        }
    }
}

/// `Unary` - `fn method(self) R`
/// `Binary` - `fn method(self, rhs: Self) R`
/// `Indexed` - `fn method(self, index: usize) R` (example: for Slice = R, SlicePtr = *R)
pub enum MethodShape {
    Unary,
    Binary,
    Indexed,
}
