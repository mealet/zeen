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
}
