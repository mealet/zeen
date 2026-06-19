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

            DeclarationKind::StructDecl {
                name,
                is_pub,
                generics,
                fields,
                methods,
            } => {
                let self_def = def_id.expect("StructDecl must have DefId, something went wrong");

                let hir_fields: Vec<HirField> = fields
                    .iter()
                    .map(|field| HirField {
                        def_id: self.resolution.def_of_field(field).unwrap_or(self_def),
                        name: field.name,
                        ty: Rc::new(self.lower_type(field.ty)),
                        is_pub: field.is_pub,
                    })
                    .collect();

                let hir_methods: Vec<Rc<HirDecl>> = methods
                    .iter()
                    .filter_map(|method| self.lower_decl_as_method(method, Some(self_def)))
                    .collect();

                HirDeclKind::Struct(Rc::new(HirStruct {
                    name,
                    is_pub,
                    generics: self.lower_generics(generics),
                    fields: hir_fields,
                    methods: hir_methods,
                }))
            }

            DeclarationKind::InterfaceDecl {
                name,
                is_pub,
                generics,
                methods,
            } => {
                let hir_methods: Vec<Rc<HirDecl>> = methods
                    .iter()
                    .filter_map(|method| self.lower_decl_as_method(method, None))
                    .collect();

                HirDeclKind::Interface(Rc::new(HirInterface {
                    name,
                    is_pub,
                    generics: self.lower_generics(generics),
                    methods: hir_methods
                }))
            }

            DeclarationKind::ImplementDecl {
                interface,
                object,
                methods
            } => {
                let object_def = self.path_expr_def_id(object);
                let interface_def = self.path_expr_def_id(interface);

                let hir_methods: Vec<Rc<HirDecl>> = methods
                    .iter()
                    .filter_map(|m| self.lower_decl_as_method(m, object_def))
                    .collect();

                HirDeclKind::Implement(Rc::new(HirImplement {
                    interface: interface_def,
                    object: object_def,
                    methods: hir_methods,
                }))
            }

            DeclarationKind::EnumDecl {
                name,
                variants,
                is_pub
            } => {
                let hir_variants: Vec<HirEnumVariant> = variants
                    .iter()
                    .map(|variant| HirEnumVariant {
                        def_id: self.resolution.def_of_variant(variant).unwrap_or(DefId(u32::MAX)),
                        name: variant.name,
                        span: variant.span
                    })
                    .collect();

                HirDeclKind::Enum(Rc::new(HirEnum {
                    name,
                    is_pub,
                    variants: hir_variants,
                }))
            }

            DeclarationKind::ExternVar { name, ty } => HirDeclKind::ExternVar {
                name,
                ty: Rc::new(self.lower_type(ty)),
            },

            DeclarationKind::ExternLink { .. } => HirDeclKind::ExternLink,
            DeclarationKind::ExternInclude { .. } => HirDeclKind::ExternInclude,
            DeclarationKind::Use { .. } => return None, 

            _ => todo!("other declarations must be implemented"),
        };

        Some(Rc::new(HirDecl {
            id: self.fresh_id(),
            def_id: def_id.unwrap_or(DefId(u32::MAX)),
            kind,
            source: decl.source.clone(),
        }))
    }

    fn lower_decl_as_method<'ctx>(
        &mut self,
        decl: &'ctx Declaration<'ctx>,
        self_def: Option<DefId>,
    ) -> Option<Rc<HirDecl>> {
        let def_id = self.def_id_of_decl(decl);

        let DeclarationKind::FnDecl { .. } = decl.kind else {
            return None;
        };

        let kind = HirDeclKind::Fn(Rc::new(self.lower_fn(decl, self_def)));

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

        let hir_params: Vec<Rc<HirParam>> = params
            .iter()
            .map(|param| Rc::new(self.lower_param(param)))
            .collect();

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
            self_param,
        }
    }

    fn lower_param(&mut self, param: &FnParam) -> HirParam {
        let def_id = self.resolution.def_of_param(param);

        HirParam {
            id: self.fresh_id(),
            def_id,
            name: param.name,
            ty: Rc::new(self.lower_type(param.ty)),
            span: param.span,
        }
    }

    fn lower_generics(&mut self, generics: Option<&[GenericType]>) -> Vec<HirGenericParam> {
        let Some(generics) = generics else {
            return Vec::new();
        };

        generics
            .iter()
            .map(|gtype| {
                let def_id = self
                    .resolution
                    .def_of_generic(gtype)
                    .unwrap_or(DefId(u32::MAX));

                let bounds = gtype
                    .interfaces
                    .map(|ifaces| {
                        ifaces
                            .iter()
                            .filter_map(|name| self.lookup_type_def_by_name(name.0))
                            .collect()
                    })
                    .unwrap_or_default();

                HirGenericParam {
                    def_id,
                    name: gtype.name,
                    bounds,
                }
            })
            .collect()
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
