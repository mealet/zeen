use zeen_ast::Source;
use zeen_resolve::DefId;

use crate::HirId;

/// Declaration in HIR (High Level Representation) version
#[derive(Debug)]
pub struct HirDecl {
    pub id: HirId,
    pub def_id: DefId,
    pub kind: HirDeclKind,

    /// Source contains span of Declaration and ref to the current module source code
    pub source: Source,
}

#[derive(Debug)]
pub enum HirDeclKind {}
