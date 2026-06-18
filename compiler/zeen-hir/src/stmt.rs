use zeen_ast::Source;
use zeen_resolve::DefId;

use crate::HirId;

#[derive(Debug)]
pub struct HirStmt {
    pub id: HirId,
    pub kind: HirStmtKind,
    pub source: Source,
}

#[derive(Debug)]
pub enum HirStmtKind {}
