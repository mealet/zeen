use zeen_ast::Source;
use zeen_resolve::DefId;

use crate::HirId;

#[derive(Debug)]
pub struct HirTypeExpr {
    pub id: HirId,
    pub kind: HirTypeKind,
    pub source: Source,
}

#[derive(Debug)]
pub enum HirTypeKind {}
