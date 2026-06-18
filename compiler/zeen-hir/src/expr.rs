use zeen_ast::Source;
use zeen_resolve::DefId;

use crate::HirId;

#[derive(Debug)]
pub struct HirExpr {
    pub id: HirId,
    pub kind: HirExprKind,
    pub source: Source,
}

#[derive(Debug)]
pub enum HirExprKind {}
