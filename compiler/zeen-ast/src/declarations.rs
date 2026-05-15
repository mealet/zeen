use lasso::Spur;
use miette::SourceSpan;

use crate::{expressions::Expression, statements::Statement, types::TypeExpr};

#[derive(Debug, Clone)]
pub struct Declaration<'arena> {
    kind: DeclarationKind<'arena>,
    span: SourceSpan,
}

#[derive(Debug, Clone)]
pub enum DeclarationKind<'arena> {
    FnDecl {
        name: Spur,

        generics: &'arena [GenericType<'arena>],
        params: &'arena [FnParam<'arena>],
        return_type: Option<&'arena TypeExpr<'arena>>,

        body: Option<&'arena Statement<'arena>>,

        is_pub: bool,
        is_extern: bool,
    },

    StructDecl {
        name: Spur,

        generics: &'arena [GenericType<'arena>],
        fields: &'arena [StructField<'arena>],
        methods: &'arena [Declaration<'arena>], // FnDecl

        is_pub: bool,
    },

    EnumDecl {
        name: Spur,
        variants: &'arena [EnumVariant],
        is_pub: bool,
    },

    ExternVar {
        name: Spur,
        ty: &'arena TypeExpr<'arena>,
    },

    ExternLink {
        path: Spur,
    },

    ExternInclude {
        path: Spur,
    },

    Import {
        module: Spur,
        alias: Spur,
    },
}

// Fn

#[derive(Debug)]
pub struct FnParam<'arena> {
    name: Option<Spur>,
    ty: &'arena TypeExpr<'arena>,
    span: SourceSpan,
}

#[derive(Debug)]
pub struct GenericType<'arena> {
    name: Spur,
    interfaces: Option<&'arena [Spur]>,
}

// Struct

#[derive(Debug)]
pub struct StructField<'arena> {
    name: Spur,
    ty: &'arena TypeExpr<'arena>,
    is_pub: bool,
}

// Enum

#[derive(Debug)]
pub struct EnumVariant {
    name: Spur,
    span: SourceSpan,
}
