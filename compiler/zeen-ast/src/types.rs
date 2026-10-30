use lasso::Spur;
use miette::SourceSpan;

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct TypeExpr<'arena> {
    pub kind: TypeKind<'arena>,
    pub span: SourceSpan,
}

impl TypeExpr<'_> {
    pub fn merge_span(&self, other: SourceSpan) -> SourceSpan {
        let start = self.span.offset().min(other.offset());
        let end = (self.span.offset() + self.span.len()).max(other.offset() + other.len());

        SourceSpan::new(start.into(), end - start)
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum TypeKind<'arena> {
    Builtin(BuiltinType),

    SelfType,
    SelfAlias,
    VaArgs,

    Named {
        name: Spur,
        generic_args: Option<&'arena [&'arena TypeExpr<'arena>]>,
    },

    Const(&'arena TypeExpr<'arena>),

    /// `typeof <expr>` - infers the type of an expression without evaluating it.
    TypeOf(&'arena crate::expressions::Expression<'arena>),

    Splice(&'arena crate::expressions::Expression<'arena>),

    ComptimeCall(&'arena crate::expressions::Expression<'arena>),

    /// `*T` - single element pointer
    SinglePointer(&'arena TypeExpr<'arena>),
    /// `[*]T` - C-style pointer to unknown number of elements.
    ManyPointer(&'arena TypeExpr<'arena>),

    Array {
        element: &'arena TypeExpr<'arena>,
        len: Option<&'arena crate::expressions::Expression<'arena>>,
    },

    Fn {
        params: &'arena [&'arena TypeExpr<'arena>],
        generic_args: Option<&'arena [crate::declarations::GenericType<'arena>]>,
        ret: &'arena TypeExpr<'arena>,
    },

    /// Fat function pointer: `Fn(T, ...) R` (copyable) or `FnOnce(T, ...) R`
    /// (movable).
    FatFn {
        params: &'arena [&'arena TypeExpr<'arena>],
        ret: &'arena TypeExpr<'arena>,
        once: bool,
    },
}

#[allow(non_camel_case_types)]
#[derive(Debug, PartialEq, Clone, Copy, Eq, Hash)]
pub enum BuiltinType {
    i8,
    i16,
    i32,
    i64,
    isize,

    u8,
    u16,
    u32,
    u64,
    usize,

    f32,
    f64,

    bool,
    char,
    void,
    never,
}

impl BuiltinType {
    pub fn try_lexer_type(value: zeen_lexer::token::CompilerType) -> Self {
        use zeen_lexer::token::CompilerType;

        match value {
            CompilerType::i8 => Self::i8,
            CompilerType::i16 => Self::i16,
            CompilerType::i32 => Self::i32,
            CompilerType::i64 => Self::i64,
            CompilerType::isize => Self::isize,

            CompilerType::u8 => Self::u8,
            CompilerType::u16 => Self::u16,
            CompilerType::u32 => Self::u32,
            CompilerType::u64 => Self::u64,
            CompilerType::usize => Self::usize,

            CompilerType::f32 => Self::f32,
            CompilerType::f64 => Self::f64,

            CompilerType::bool => Self::bool,
            CompilerType::char => Self::char,
            CompilerType::void => Self::void,
            CompilerType::never => Self::never,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BuiltinShape {
    Scalar(BuiltinType),
    Slice {
        element: Box<BuiltinShape>,
        is_const: bool,
    },
    ManyPointer {
        element: Box<BuiltinShape>,
        is_const: bool,
    },
    Array {
        element: Box<BuiltinShape>,
        len: u64,
    },
    Param(u32),
}

impl BuiltinShape {
    pub fn has_params(&self) -> bool {
        match self {
            Self::Scalar(_) => false,
            Self::Slice { element, .. }
            | Self::ManyPointer { element, .. }
            | Self::Array { element, .. } => element.has_params(),
            Self::Param(_) => true,
        }
    }

    pub fn used_params(&self, out: &mut Vec<u32>) {
        match self {
            Self::Scalar(_) => {}
            Self::Slice { element, .. }
            | Self::ManyPointer { element, .. }
            | Self::Array { element, .. } => element.used_params(out),
            Self::Param(index) => {
                if !out.contains(index) {
                    out.push(*index);
                }
            }
        }
    }

    pub fn key(&self) -> String {
        match self {
            Self::Scalar(builtin) => builtin.to_string(),
            Self::Slice { element, is_const } => {
                format!("[]{}{}", Self::const_prefix(*is_const), element.key())
            }
            Self::ManyPointer { element, is_const } => {
                format!("[*]{}{}", Self::const_prefix(*is_const), element.key())
            }
            Self::Array { element, len } => format!("[{len}]{}", element.key()),
            Self::Param(index) => format!("#{index}"),
        }
    }

    fn const_prefix(is_const: bool) -> &'static str {
        if is_const { "const " } else { "" }
    }
}

impl std::fmt::Display for BuiltinType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::i8 => "i8",
                Self::i16 => "i16",
                Self::i32 => "i32",
                Self::i64 => "i64",
                Self::isize => "isize",

                Self::u8 => "u8",
                Self::u16 => "u16",
                Self::u32 => "u32",
                Self::u64 => "u64",
                Self::usize => "usize",

                Self::f32 => "f32",
                Self::f64 => "f64",

                Self::bool => "bool",
                Self::char => "char",
                Self::void => "void",
                Self::never => "never",
            }
        )
    }
}
