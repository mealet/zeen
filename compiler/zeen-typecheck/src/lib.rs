#![allow(unused)]

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use lasso::Spur;
use miette::SourceSpan;

use crate::{
    coerce::{try_coerce, CoerceResult},
    result::{CallResolution, TypeCheckResult},
    types::{Capabilities, StructTypeInfo, Type, TypeId, TypeInterner},
    context::{FnCtx, TypeCheckCtx},
};

use zeen_ast::{
    expressions::{BinaryOp, UnaryOp, Literal},
    types::BuiltinType,
};
use zeen_hir::{
    decl::{HirDecl, HirDeclKind, HirFn},
    expr::{HirExpr, HirExprKind, HirFieldInit, HirMacroKind},
    stmt::{HirStmt, HirStmtKind},
    types::{HirTypeExpr, HirTypeKind},
    HirId, HirModule,
};
use zeen_resolve::{DefId, DefKind, ResolutionResult};

mod coerce;
mod context;
mod error;
mod types;
mod result;
