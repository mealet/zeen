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

// TODO: Create HIR tree and do better public exports
