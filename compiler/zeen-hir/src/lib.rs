#![allow(unused)]

use std::rc::Rc;

use lasso::Spur;
use miette::SourceSpan;

use zeen_ast::Source;
use zeen_resolve::DefId;

pub mod decl;
pub mod expr;
pub mod stmt;
pub mod types;

/// Unique identifier for each HIR node
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HirId(pub u32);

/// Container of program's declarations references
#[derive(Debug)]
pub struct HirModule {
    pub decls: Vec<Rc<decl::HirDecl>>,
}

// ====| Public Exports |====

pub use decl::{
    HirDecl, HirDeclKind, HirEnum, HirEnumVariant, HirField, HirFn, HirGenericParam, HirImplement,
    HirInterface, HirParam, HirStruct,
};

pub use expr::{HirExpr, HirExprKind, HirFieldInit};
pub use stmt::{HirStmt, HirStmtKind};
pub use types::{HirTypeExpr, HirTypeKind};
