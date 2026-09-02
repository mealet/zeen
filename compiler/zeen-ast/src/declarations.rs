use lasso::Spur;
use miette::SourceSpan;

use crate::{Source, expressions::Expression, statements::Statement, types::TypeExpr};

#[derive(Debug, Clone, PartialEq)]
pub struct Declaration<'arena> {
    pub kind: DeclarationKind<'arena>,
    pub source: Source,
}

impl Declaration<'_> {
    pub fn merge_span(&self, other: SourceSpan) -> SourceSpan {
        let start = self.source.span.offset().min(other.offset());
        let end =
            (self.source.span.offset() + self.source.span.len()).max(other.offset() + other.len());

        SourceSpan::new(start.into(), end - start)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DeclarationKind<'arena> {
    FnDecl {
        name: (Spur, SourceSpan),

        generics: Option<&'arena [GenericType<'arena>]>,
        params: &'arena [FnParam<'arena>],
        return_type: Option<&'arena TypeExpr<'arena>>,

        body: Option<&'arena Statement<'arena>>,

        is_pub: bool,
        is_extern: bool,
    },

    StructDecl {
        name: (Spur, SourceSpan),
        is_pub: bool,

        generics: Option<&'arena [GenericType<'arena>]>,
        fields: &'arena [StructField<'arena>],
        methods: &'arena [&'arena Declaration<'arena>], // FnDecl
    },

    InterfaceDecl {
        name: (Spur, SourceSpan),
        is_pub: bool,

        generics: Option<&'arena [GenericType<'arena>]>,
        methods: &'arena [&'arena Declaration<'arena>], // FnDecl
    },

    ImplementDecl {
        interface: (Spur, SourceSpan),
        object: (Spur, SourceSpan, &'arena [&'arena TypeExpr<'arena>]), // name, span, generics slots
        generics: Option<&'arena [GenericType<'arena>]>,

        methods: &'arena [&'arena Declaration<'arena>], // FnDecl
    },

    EnumDecl {
        name: (Spur, SourceSpan),
        variants: &'arena [EnumVariant],
        is_pub: bool,
    },

    ExternVar {
        name: (Spur, SourceSpan),
        ty: &'arena TypeExpr<'arena>,
    },

    GlobalVar {
        name: (Spur, SourceSpan),
        ty: &'arena TypeExpr<'arena>,
        value: &'arena Expression<'arena>,
        is_const: bool,
        is_pub: bool,
    },

    ExternLink {
        path: Spur,
    },

    ExternInclude {
        path: Spur,
    },

    Use {
        module: (Spur, SourceSpan),
    },

    Alias(AliasDecl<'arena>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AliasDecl<'arena> {
    pub name: (Spur, SourceSpan),
    pub is_pub: bool,
    pub generics: Option<&'arena [GenericType<'arena>]>,
    pub ty: &'arena TypeExpr<'arena>,
}

// Fn

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FnParam<'arena> {
    pub name: Option<Spur>,
    pub ty: &'arena TypeExpr<'arena>,
    pub span: SourceSpan,
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub struct GenericType<'arena> {
    pub name: (Spur, SourceSpan),
    pub interfaces: Option<&'arena [(Spur, SourceSpan)]>,
}

// Struct

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StructField<'arena> {
    pub name: Spur,
    pub ty: &'arena TypeExpr<'arena>,
    pub is_pub: bool,
}

// Enum

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnumVariant {
    pub name: Spur,
    pub span: SourceSpan,
}
