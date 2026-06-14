use lasso::Spur;
use miette::SourceSpan;

use crate::{expressions::Expression, statements::Statement, types::TypeExpr};

#[derive(Debug, Clone, Copy)]
pub struct Declaration<'arena> {
    pub kind: DeclarationKind<'arena>,
    pub span: SourceSpan,
}

impl Declaration<'_> {
    pub fn merge_span(&self, other: SourceSpan) -> SourceSpan {
        let start = self.span.offset().min(other.offset());
        let end = (self.span.offset() + self.span.len()).max(other.offset() + other.len());

        SourceSpan::new(start.into(), end - start)
    }
}

#[derive(Debug, Clone, Copy)]
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
        interface: &'arena Expression<'arena>, // must be ident / field access that ends with ident.
        object: &'arena Expression<'arena>,    // here too

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

    ExternLink {
        path: Spur,
    },

    ExternInclude {
        path: Spur,
    },

    Use {
        module: (Spur, SourceSpan),
    },
}

// Fn

#[derive(Debug, Clone, Copy)]
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

#[derive(Debug, Clone, Copy)]
pub struct StructField<'arena> {
    pub name: Spur,
    pub ty: &'arena TypeExpr<'arena>,
    pub is_pub: bool,
}

// Enum

#[derive(Debug, Clone, Copy)]
pub struct EnumVariant {
    pub name: Spur,
    pub span: SourceSpan,
}
