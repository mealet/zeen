#![allow(unused)]

use std::rc::Rc;

use lasso::Spur;
use miette::SourceSpan;

use zeen_ast::{
    declarations::{Declaration, DeclarationKind, FnParam, GenericType},
    expressions::{Expression, ExpressionKind},
    statements::{Statement, StatementKind},
    types::{TypeExpr, TypeKind},
};
use zeen_resolve::{DefId, DefKind, NodeKey, Resolution, ResolutionResult};

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

// No, this is not AI generated, comments are made by me,
// this just looks fine for me.

// =========| Public Exports |=========

pub use decl::{
    HirDecl, HirDeclKind, HirEnum, HirEnumVariant, HirField, HirFn, HirGenericParam, HirImplement,
    HirInterface, HirParam, HirStruct,
};

pub use expr::{HirExpr, HirExprKind, HirFieldInit};
pub use stmt::{HirStmt, HirStmtKind};
pub use types::{HirTypeExpr, HirTypeKind};

// =========| HIR Lowering |=========

pub struct HirLowering<'res> {
    resolution: &'res ResolutionResult,
    next_id: u32,
}

impl<'res> HirLowering<'res> {
    pub fn new(resolution: &'res ResolutionResult) -> Self {
        Self {
            resolution,
            next_id: 0,
        }
    }

    fn fresh_id(&mut self) -> HirId {
        let id = HirId(self.next_id);
        self.next_id += 1;
        id
    }

    // ==> Helpers

    fn resolution_of_stmt(&self, stmt: &Statement) -> Option<Resolution> {
        self.resolution
            .expr_bindings
            .get(&NodeKey::from_stmt(stmt))
            .copied()
    }

    fn def_id_of_decl(&self, decl: &Declaration) -> Option<DefId> {
        let key = NodeKey::from_decl(decl);

        self.resolution.binding_sites.get(&key).copied()
    }

    fn lookup_type_def_by_name(&self, name: Spur) -> Option<DefId> {
        self.resolution
            .defs
            .iter()
            .find(|(_, info)| {
                info.name == name
                    && matches!(
                        info.kind,
                        DefKind::Interface | DefKind::Struct | DefKind::Enum
                    )
            })
            .map(|(id, _)| *id)
    }

    fn path_expr_def_id(&self, expr: &Expression) -> Option<DefId> {
        let target = match expr.kind {
            ExpressionKind::FieldAccess { field, .. } => field,
            _ => expr,
        };

        match self.resolution.resolution_of_expr(target) {
            Some(Resolution::Def(id)) => Some(id),
            _ => None,
        }
    }

    // ==> Entry Point

    pub fn lower_module<'ctx>(&mut self, decls: &'ctx [&'ctx Declaration<'ctx>]) -> HirModule {
        let decls = decls
            .iter()
            .filter_map(|decl| self.lower_decl(decl))
            .collect();

        HirModule { decls }
    }

    // ==> Lowering Functions

    // > Declarations

    fn lower_decl<'ctx>(&mut self, decl: &'ctx Declaration<'ctx>) -> Option<Rc<HirDecl>> {
        let def_id = self.def_id_of_decl(decl);

        let kind = match decl.kind {
            DeclarationKind::FnDecl { .. } => HirDeclKind::Fn(Rc::new(self.lower_fn(decl, None))),

            _ => todo!("other declarations must be implemented"),
        };

        Some(Rc::new(HirDecl {
            id: self.fresh_id(),
            def_id: def_id.unwrap_or(DefId(u32::MAX)),
            kind,
            source: decl.source.clone(),
        }))
    }

    fn lower_fn<'ctx>(&mut self, decl: &'ctx Declaration<'ctx>, self_def: Option<DefId>) -> HirFn {
        let DeclarationKind::FnDecl {
            name,
            generics,
            params,
            return_type,
            body,
            is_pub,
            is_extern,
        } = decl.kind
        else {
            unreachable!("lower_fn called on non-FnDecl")
        };

        let hir_params: Vec<Rc<HirParam>> =
            params.iter().map(|param| Rc::new(self.lower_param(param))).collect();

        let self_param = if self_def.is_some() {
            hir_params
                .iter()
                .find(|param| matches!(param.ty.kind, HirTypeKind::SelfType(_)))
                .and_then(|param| param.def_id)
        } else {
            None
        };

        HirFn {
            name,
            generics: self.lower_generics(generics),
            params: hir_params,
            return_type: return_type.map(|ty| Rc::new(self.lower_type(ty))),
            body: body.map(|stmt| Rc::new(self.lower_stmt(stmt))),
            is_pub,
            is_extern,
            self_param
        }
    }

    fn lower_param(&mut self, param: &FnParam) -> HirParam {
        let def_id = self.resolution.def_of_param(param);
        
        HirParam {
            id: self.fresh_id(),
            def_id,
            name: param.name,
            ty: Rc::new(self.lower_type(param.ty)),
            span: param.span
        }
    }

    fn lower_generics(&mut self, generics: Option<&[GenericType]>) -> Vec<HirGenericParam> {
        todo!()
    }

    // > Statements

    fn lower_stmt<'ctx>(&mut self, stmt: &'ctx Statement<'ctx>) -> HirStmt {
        todo!()
    }

    // > Expressions

    fn lower_expr<'ctx>(&mut self, expr: &'ctx Expression<'ctx>) -> HirExpr {
        todo!()
    }

    // > Types

    fn lower_type<'ctx>(&mut self, ty: &'ctx TypeExpr<'ctx>) -> HirTypeExpr {
        todo!()
    }
}
