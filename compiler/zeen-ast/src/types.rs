use lasso::Spur;
use miette::SourceSpan;

#[derive(Debug, PartialEq)]
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

#[derive(Debug, PartialEq)]
pub enum TypeKind<'arena> {
    Builtin(BuiltinType),

    SelfType,
    SelfAlias,

    Named {
        name: Spur,
        generic_args: Option<&'arena [&'arena TypeExpr<'arena>]>,
    },

    Const(&'arena TypeExpr<'arena>),
    Pointer(&'arena TypeExpr<'arena>),

    Array {
        element: &'arena TypeExpr<'arena>,
        len: Option<&'arena crate::expressions::Expression<'arena>>,
    },

    Fn {
        params: &'arena [&'arena TypeExpr<'arena>],
        generic_args: Option<&'arena [&'arena TypeExpr<'arena>]>,
        ret: &'arena TypeExpr<'arena>,
    },
}

#[allow(non_camel_case_types)]
#[derive(Debug, PartialEq)]
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
        }
    }
}
