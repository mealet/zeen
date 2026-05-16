use lasso::Spur;
use miette::SourceSpan;

#[derive(Debug)]
pub struct TypeExpr<'arena> {
    pub kind: TypeKind<'arena>,
    pub span: SourceSpan,
}

#[derive(Debug)]
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
#[derive(Debug)]
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
