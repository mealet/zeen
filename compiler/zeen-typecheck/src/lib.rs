#![allow(unused)]

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use lasso::Spur;
use miette::SourceSpan;

use crate::error::TypeError;
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

pub struct TypeChecker<'res> {
    resolution: &'res ResolutionResult,

    result: TypeCheckResult,
    ctx: TypeCheckCtx,

    fn_sigs: HashMap<DefId, FnSignature>
}

struct FnSignature {
    params: Vec<TypeId>,
    ret: TypeId,
    generics: Vec<DefId>,
}

impl<'res> TypeChecker<'res> {
    pub fn new(resolution: &'res ResolutionResult) -> Self {
        Self {
            resolution,
            result: TypeCheckResult::default(),
            ctx: TypeCheckCtx::new(),
            fn_sigs: HashMap::new(),
        }
    }

    pub fn finish(self) -> TypeCheckResult {
        self.result
    }

    // --> Helpers

    fn def_kind(&self, def_id: DefId) -> Option<&DefKind> {
        self.resolution.defs.get(&def_id).map(|info| &info.kind)
    }

    fn report(&mut self, err: TypeError) {
        self.result.errors.push(err);
    }

    // --> Entry Point

    pub fn check_module(&mut self, module: &HirModule) {
        // Few words here: we're doing multiple passes here:
        // 1. Declare signatures
        // 2. Check and infer if structs have Copy and Drop capabilities
        // 3. Check declarations bodies

        for decl in &module.decls {
            self.declare_signature(decl);
        }

        for decl in &module.decls {
            self.compute_structs_capabilities(decl);
        }

        for decl in &module.decls {
            self.check_decl_body(decl);
        }
    }

    // > Pass 1

    fn declare_signature(&mut self, decl: &HirDecl) {
        todo!()
    }

    // > Pass 2

    fn compute_structs_capabilities(&mut self, decl: &HirDecl) {
        todo!()
    }

    // > Pass 3

    fn check_decl_body(&mut self, decl: &HirDecl) {
        todo!()
    }
}
