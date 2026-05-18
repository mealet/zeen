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

    Named(Spur),

    Const(&'arena TypeExpr<'arena>),
    Pointer(&'arena TypeExpr<'arena>),

    Array {
        element: &'arena TypeExpr<'arena>,
        len: &'arena crate::expressions::Expression<'arena>,
    },

    Fn {
        params: &'arena [TypeExpr<'arena>],
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
    pub fn try_str(slice: &str) -> Option<Self> {
        match slice {
            "i8" => Some(Self::i8),
            "i16" => Some(Self::i16),
            "i32" => Some(Self::i32),
            "i64" => Some(Self::i64),
            "isize" => Some(Self::isize),

            "u8" => Some(Self::u8),
            "u16" => Some(Self::u16),
            "u32" => Some(Self::u32),
            "u64" => Some(Self::u64),
            "usize" => Some(Self::usize),

            "f32" => Some(Self::f32),
            "f64" => Some(Self::f64),

            "bool" => Some(Self::bool),
            "char" => Some(Self::bool),
            "void" => Some(Self::void),

            _ => None,
        }
    }
}
