/// Compiler reserved interfaces names.
#[derive(Debug, Clone, Copy, PartialEq, Hash, Eq)]
pub enum WellKnownInterface {
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

impl WellKnownInterface {
    pub const ALL: &'static [WellKnownInterface] = &[
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

            Self::Copy => "",
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
            Self::Display | Self::Debug | Self::Neg | Self::Not | Self::BitNot | Self::Deref => {
                MethodShape::Unary
            }

            Self::DerefPtr => MethodShape::UnaryPtr,
            Self::Drop => MethodShape::UnaryVoid,
            Self::Copy => MethodShape::Empty,

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

            Self::Slice => MethodShape::Indexed,
            Self::SlicePtr => MethodShape::IndexedPtr,
        }
    }
}

/// `Unary` - `fn method(self) R`
/// `UnaryVoid` - `fn method(self)`
/// `UnaryPtr` - `fn method(self) *R`
/// `Binary` - `fn method(self, rhs: Self) R`
/// `Indexed` - `fn method(self, index: usize) R`
/// `IndexedPtr` - `fn method(self, index: usize) *R`
/// `Empty` - no method required (for Copy interface)
pub enum MethodShape {
    Unary,
    UnaryVoid,
    UnaryPtr,
    Binary,
    Indexed,
    IndexedPtr,
    Empty,
}
