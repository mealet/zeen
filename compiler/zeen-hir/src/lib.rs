use std::{cell::RefCell, rc::Rc, sync::Arc};

use lasso::{Rodeo, Spur};
use smol_str::SmolStr;

use zeen_ast::{
    declarations::{Declaration, DeclarationKind, FnParam, GenericType},
    expressions::{Expression, ExpressionKind},
    statements::{Statement, StatementKind},
    types::{TypeExpr, TypeKind},
};
use zeen_resolve::{BindingSlotKey, DefId, DefKind, NodeKey, Resolution, ResolutionResult};

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

pub use expr::{HirExpr, HirExprKind, HirFieldInit, HirMacroKind};
pub use stmt::{HirStmt, HirStmtKind};
pub use types::{HirTypeExpr, HirTypeKind};

// =========| HIR Lowering |=========

pub struct HirLowering<'res> {
    resolution: &'res ResolutionResult,
    interner: Rc<RefCell<Rodeo>>,

    next_id: u32,
    current_src: miette::NamedSource<Arc<String>>,
}

impl<'res> HirLowering<'res> {
    pub fn new(resolution: &'res ResolutionResult, interner: Rc<RefCell<Rodeo>>) -> Self {
        Self {
            resolution,
            interner,
            next_id: 0,
            current_src: miette::NamedSource::new("", Arc::new("".into())),
        }
    }

    fn fresh_id(&mut self) -> HirId {
        let id = HirId(self.next_id);
        self.next_id += 1;
        id
    }

    fn interner_resolve(&self, key: &Spur) -> SmolStr {
        let interner = self.interner.borrow();
        let resolved = interner.resolve(key);
        resolved.into()
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
            Some(Resolution::SelfType(id)) => Some(id),
            _ => None,
        }
    }

    fn resolve_macro_kind(&self, name: Spur) -> HirMacroKind {
        let name = self.interner_resolve(&name);

        match name.as_str() {
            "as" => HirMacroKind::As,
            "sizeof" => HirMacroKind::SizeOf,
            "alignof" => HirMacroKind::AlignOf,
            "typename" => HirMacroKind::TypeName,

            "print" => HirMacroKind::Print,
            "println" => HirMacroKind::Println,
            "format" => HirMacroKind::Format,

            "panic" => HirMacroKind::Panic,
            "unreachable" => HirMacroKind::Unreachable,
            "todo" => HirMacroKind::Todo,

            "dbg" => HirMacroKind::Dbg,
            "uninit" => HirMacroKind::Uninit,

            _ => HirMacroKind::Unknown,
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
        self.current_src = decl.source.src();

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
                    methods: hir_methods,
                }))
            }

            DeclarationKind::ImplementDecl {
                interface: _,
                object,
                methods,
                generics,
            } => {
                let (interface_def, object_def) = match self
                    .resolution
                    .implement_names
                    .get(&NodeKey::from_decl(decl))
                {
                    Some((iface_res, obj_res)) => (
                        if let Resolution::Def(id) = iface_res {
                            Some(id)
                        } else {
                            None
                        },
                        if let Resolution::Def(id) = obj_res {
                            Some(id)
                        } else {
                            None
                        },
                    ),

                    None => (None, None),
                };

                let hir_generics = self.lower_generics(generics);

                let object_bindings: Vec<DefId> = object
                    .2
                    .iter()
                    .enumerate()
                    .map(|(i, _)| {
                        match self
                            .resolution
                            .implement_generic_bindings
                            .get(&BindingSlotKey(decl as *const _ as usize, i))
                        {
                            Some(Resolution::Def(id)) => *id,
                            _ => DefId(u32::MAX),
                        }
                    })
                    .collect();

                let object_bindings_span = object.2.iter().skip(1).fold(
                    object.2.first().map(|slot| slot.span).unwrap_or(object.1),
                    |acc, slot| {
                        let start = acc.offset().min(slot.span.offset());
                        let end =
                            (acc.offset() + acc.len()).max(slot.span.offset() + slot.span.len());
                        miette::SourceSpan::new(start.into(), end - start)
                    },
                );

                let object_generic_types: Vec<Rc<HirTypeExpr>> = object
                    .2
                    .iter()
                    .map(|slot| Rc::new(self.lower_type(slot)))
                    .collect();

                let hir_methods: Vec<Rc<HirDecl>> = methods
                    .iter()
                    .filter_map(|m| self.lower_decl_as_method(m, object_def.copied()))
                    .collect();

                HirDeclKind::Implement(Rc::new(HirImplement {
                    generics: hir_generics,
                    interface: interface_def.copied(),
                    object: object_def.copied(),
                    object_generics_bindings: object_bindings,
                    object_generic_types,
                    object_bindings_span,
                    methods: hir_methods,
                }))
            }

            DeclarationKind::EnumDecl {
                name,
                variants,
                is_pub,
            } => {
                let hir_variants: Vec<HirEnumVariant> = variants
                    .iter()
                    .map(|variant| HirEnumVariant {
                        def_id: self
                            .resolution
                            .def_of_variant(variant)
                            .unwrap_or(DefId(u32::MAX)),
                        name: variant.name,
                        span: variant.span,
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

            DeclarationKind::GlobalVar {
                name,
                ty,
                value,
                is_const,
                is_pub,
            } => HirDeclKind::GlobalVar {
                name,
                ty: Rc::new(self.lower_type(ty)),
                value: Rc::new(self.lower_expr(value)),
                is_const,
                is_pub,
            },

            DeclarationKind::ExternLink { .. } => HirDeclKind::ExternLink,
            DeclarationKind::ExternInclude { .. } => HirDeclKind::ExternInclude,
            DeclarationKind::Use { .. } => return None,
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

        let def_id = self.def_id_of_decl(decl).unwrap_or(DefId(u32::MAX));
        let parent_fn = self.resolution.nested_fn_parents.get(&def_id).copied();

        HirFn {
            name,
            generics: self.lower_generics(generics),
            params: hir_params,
            return_type: return_type.map(|ty| Rc::new(self.lower_type(ty))),
            body: body.map(|stmt| Rc::new(self.lower_stmt(stmt))),
            is_pub,
            is_extern,
            self_param,
            parent_fn,
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
        let kind = match stmt.kind {
            StatementKind::Let {
                name,
                explicit_type,
                value,
                is_const,
            } => {
                let def_id = match self.resolution_of_stmt(stmt) {
                    Some(Resolution::Def(id)) => id,
                    _ => DefId(u32::MAX),
                };

                HirStmtKind::Let {
                    def_id,
                    name,
                    explicit_type: explicit_type.map(|t| Rc::new(self.lower_type(t))),
                    value: value.map(|v| Rc::new(self.lower_expr(v))),
                    is_const,
                }
            }

            StatementKind::Assign { object, value } => HirStmtKind::Assign {
                object: Rc::new(self.lower_expr(object)),
                value: Rc::new(self.lower_expr(value)),
            },

            StatementKind::CompoundAssign { object, value, op } => HirStmtKind::CompoundAssign {
                object: Rc::new(self.lower_expr(object)),
                value: Rc::new(self.lower_expr(value)),
                op,
            },

            StatementKind::Return { value } => HirStmtKind::Return {
                value: value.map(|val| Rc::new(self.lower_expr(val))),
            },

            StatementKind::Break => HirStmtKind::Break,
            StatementKind::Continue => HirStmtKind::Continue,

            StatementKind::While { condition, block } => HirStmtKind::While {
                condition: Rc::new(self.lower_expr(condition)),
                block: Rc::new(self.lower_stmt(block)),
            },

            StatementKind::For {
                varname,
                iterator,
                block,
            } => {
                let def_id = match self.resolution_of_stmt(stmt) {
                    Some(Resolution::Def(id)) => id,
                    _ => DefId(u32::MAX),
                };

                HirStmtKind::For {
                    def_id,
                    varname,
                    iterator: Rc::new(self.lower_expr(iterator)),
                    block: Rc::new(self.lower_stmt(block)),
                }
            }

            StatementKind::Expr(expr) => HirStmtKind::Expr(Rc::new(self.lower_expr(expr))),

            StatementKind::FnDecl(decl) => {
                let hir_decl = self
                    .lower_decl(decl)
                    .expect("nested fn declaration must lower to HIR");
                HirStmtKind::FnDecl(hir_decl)
            }

            StatementKind::TrailingExpr(_) => unreachable!(),
        };

        HirStmt {
            id: self.fresh_id(),
            kind,
            source: (stmt.span, self.current_src.clone()).into(),
        }
    }

    // > Expressions

    fn lower_expr<'ctx>(&mut self, expr: &'ctx Expression<'ctx>) -> HirExpr {
        let kind = match expr.kind {
            ExpressionKind::Literal(lit) => HirExprKind::Literal(lit),

            ExpressionKind::Ident { generic_args, .. } => {
                let resolution = self.resolution.resolution_of_expr(expr);

                let base = match resolution {
                    Some(Resolution::Def(id)) => HirExprKind::VarRef(id),
                    Some(Resolution::GenericParam(id)) => HirExprKind::GenericParamRef(id),
                    Some(Resolution::SelfValue(id)) => HirExprKind::SelfValue(id),
                    Some(Resolution::SelfType(_)) => HirExprKind::Error,
                    Some(Resolution::Builtin) | Some(Resolution::Error) | None => {
                        HirExprKind::Error
                    }
                };

                let _ = generic_args;

                base
            }

            ExpressionKind::Binary { lhs, rhs, op } => HirExprKind::Binary {
                lhs: Rc::new(self.lower_expr(lhs)),
                rhs: Rc::new(self.lower_expr(rhs)),
                op,
            },

            ExpressionKind::Unary { expr, op } => HirExprKind::Unary {
                expr: Rc::new(self.lower_expr(expr)),
                op,
            },

            ExpressionKind::Call { callee, args } => {
                let generic_args = match callee.kind {
                    ExpressionKind::Ident {
                        generic_args: Some(gargs),
                        ..
                    } => gargs.iter().map(|t| Rc::new(self.lower_type(t))).collect(),
                    _ => Vec::new(),
                };

                HirExprKind::Call {
                    callee: Rc::new(self.lower_expr(callee)),
                    args: args.iter().map(|a| Rc::new(self.lower_expr(a))).collect(),
                    generic_args,
                }
            }

            ExpressionKind::MacroCall { name, args } => HirExprKind::MacroCall {
                kind: (self.resolve_macro_kind(name.0), name.1),
                args: args
                    .iter()
                    .map(|arg| Rc::new(self.lower_expr(arg)))
                    .collect(),
            },

            ExpressionKind::If {
                condition,
                then_block,
                else_block,
            } => HirExprKind::If {
                condition: Rc::new(self.lower_expr(condition)),
                then_block: Rc::new(self.lower_stmt(then_block)),
                else_block: else_block.map(|b| Rc::new(self.lower_stmt(b))),
            },

            ExpressionKind::Switch { .. } => HirExprKind::Switch,

            ExpressionKind::FieldAccess { object, field } => {
                let (field_name, field_span) = match field.kind {
                    ExpressionKind::Ident { name, .. } => (name, field.span),
                    _ => {
                        return HirExpr {
                            id: self.fresh_id(),
                            kind: HirExprKind::Error,
                            source: (expr.span, self.current_src.clone()).into(),
                        };
                    }
                };

                HirExprKind::FieldAccess {
                    object: Rc::new(self.lower_expr(object)),
                    field: (field_name, field_span),
                }
            }

            ExpressionKind::SliceAccess { object, index } => HirExprKind::SliceAccess {
                object: Rc::new(self.lower_expr(object)),
                index: Rc::new(self.lower_expr(index)),
            },

            ExpressionKind::StructInit { ty, fields } => {
                let ty_def = self.path_expr_def_id(ty);

                let hir_fields: Vec<HirFieldInit> = fields
                    .map(|fields| {
                        fields
                            .iter()
                            .map(|f| HirFieldInit {
                                name: f.name,
                                span: f.span,
                                value: Rc::new(self.lower_expr(f.value)),
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let generic_args = self.generic_args_of_expr(ty);

                HirExprKind::StructInit {
                    ty: (ty_def, ty.span),
                    generic_args,
                    fields: hir_fields,
                }
            }

            ExpressionKind::ArrayInit { elements } => HirExprKind::ArrayInit {
                elements: elements
                    .iter()
                    .map(|e| Rc::new(self.lower_expr(e)))
                    .collect(),
            },

            ExpressionKind::ArrayRepeatInit { element, len } => HirExprKind::ArrayRepeatInit {
                element: Rc::new(self.lower_expr(element)),
                len: Rc::new(self.lower_expr(len)),
            },

            ExpressionKind::Block { stmts, trailing } => HirExprKind::Block {
                stmts: stmts
                    .iter()
                    .map(|stmt| Rc::new(self.lower_stmt(stmt)))
                    .collect(),
                trailing: trailing.map(|expr| Rc::new(self.lower_expr(expr))),
            },

            ExpressionKind::Type(ty) => HirExprKind::Type(Rc::new(self.lower_type(ty))),

            ExpressionKind::Closure {
                params,
                return_type,
                body,
            } => {
                let def_id = match self.resolution.resolution_of_expr(expr) {
                    Some(Resolution::Def(id)) => id,
                    _ => DefId(u32::MAX),
                };

                let name = self
                    .resolution
                    .defs
                    .get(&def_id)
                    .map(|info| info.name)
                    .unwrap_or_else(|| self.interner.borrow_mut().get_or_intern("<closure>"));

                let parent_fn = self.resolution.nested_fn_parents.get(&def_id).copied();

                let hir_params: Vec<Rc<HirParam>> = params
                    .iter()
                    .map(|param| Rc::new(self.lower_param(param)))
                    .collect();

                let def = Rc::new(HirFn {
                    name: (name, expr.span),
                    generics: Vec::new(),
                    params: hir_params,
                    return_type: return_type.map(|ty| Rc::new(self.lower_type(ty))),
                    body: Some(Rc::new(self.lower_stmt(body))),
                    is_pub: false,
                    is_extern: false,
                    self_param: None,
                    parent_fn,
                });

                HirExprKind::Closure { def_id, def }
            }
        };

        HirExpr {
            id: self.fresh_id(),
            kind,
            source: (expr.span, self.current_src.clone()).into(),
        }
    }

    fn generic_args_of_expr(&mut self, expr: &Expression) -> Vec<Rc<HirTypeExpr>> {
        let ExpressionKind::Ident {
            generic_args: Some(args),
            ..
        } = expr.kind
        else {
            return Vec::new();
        };

        args.iter()
            .map(|ty_expr| Rc::new(self.lower_type(ty_expr)))
            .collect()
    }

    // > Types

    fn lower_type<'ctx>(&mut self, ty: &'ctx TypeExpr<'ctx>) -> HirTypeExpr {
        let kind = match ty.kind {
            TypeKind::Builtin(b) => HirTypeKind::Builtin(b),
            TypeKind::VaArgs => HirTypeKind::VaArgs,

            TypeKind::SelfType | TypeKind::SelfAlias => {
                match self.resolution.resolution_of_type(ty) {
                    Some(Resolution::SelfType(id)) if matches!(ty.kind, TypeKind::SelfType) => {
                        HirTypeKind::SelfType(id)
                    }
                    Some(Resolution::SelfType(id)) => HirTypeKind::SelfAlias(id),
                    _ => HirTypeKind::Error,
                }
            }

            TypeKind::Named { generic_args, .. } => {
                let def_id = match self.resolution.resolution_of_type(ty) {
                    Some(Resolution::Def(id)) | Some(Resolution::GenericParam(id)) => id,
                    _ => {
                        return HirTypeExpr {
                            id: self.fresh_id(),
                            kind: HirTypeKind::Error,
                            source: (ty.span, self.current_src.clone()).into(),
                        };
                    }
                };

                let hir_args = generic_args
                    .map(|args| args.iter().map(|t| Rc::new(self.lower_type(t))).collect())
                    .unwrap_or_default();

                HirTypeKind::Named {
                    def_id,
                    generic_args: hir_args,
                }
            }

            TypeKind::Const(inner) => HirTypeKind::Const(Rc::new(self.lower_type(inner))),

            TypeKind::TypeOf(expr) => HirTypeKind::TypeOf(Rc::new(self.lower_expr(expr))),

            TypeKind::SinglePointer(inner) => {
                HirTypeKind::SinglePointer(Rc::new(self.lower_type(inner)))
            }

            TypeKind::ManyPointer(inner) => {
                HirTypeKind::ManyPointer(Rc::new(self.lower_type(inner)))
            }

            TypeKind::Array { element, len } => HirTypeKind::Array {
                element: Rc::new(self.lower_type(element)),
                len: len.map(|e| Rc::new(self.lower_expr(e))),
            },

            TypeKind::Fn {
                params,
                generic_args,
                ret,
            } => HirTypeKind::Fn {
                params: params.iter().map(|p| Rc::new(self.lower_type(p))).collect(),
                generics: self.lower_generics(generic_args),
                ret: Rc::new(self.lower_type(ret)),
            },

            TypeKind::FatFn { params, ret, once } => HirTypeKind::FatFn {
                params: params.iter().map(|p| Rc::new(self.lower_type(p))).collect(),
                ret: Rc::new(self.lower_type(ret)),
                once,
            },
        };

        HirTypeExpr {
            id: self.fresh_id(),
            kind,
            source: (ty.span, self.current_src.clone()).into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decl::HirDeclKind;
    use crate::expr::HirExprKind;
    use crate::stmt::HirStmtKind;
    use crate::types::HirTypeKind;
    use bumpalo::Bump;
    use lasso::Rodeo;
    use std::{
        cell::RefCell,
        collections::HashSet,
        path::{Path, PathBuf},
        rc::Rc,
        sync::Arc,
    };
    use zeen_ast::expressions::BinaryOp;
    use zeen_ast::types::BuiltinType;
    use zeen_driver::{CompilationContext, CompilationMode, CompilationOutput, PathsConfig};
    use zeen_parser::Parser;

    const CORE_OPS: &str = include_str!("../../../lib/core/ops.zn");

    #[derive(Debug)]
    struct Fixture {
        rodeo: Rc<RefCell<Rodeo>>,
        module: HirModule,
    }

    impl Fixture {
        fn name(&self, spur: Spur) -> String {
            self.rodeo.borrow().resolve(&spur).to_string()
        }

        fn struct_decl(&self, name: &str) -> Rc<HirStruct> {
            self.find_by_name(name)
                .and_then(|kind| match kind {
                    HirDeclKind::Struct(s) => Some(s.clone()),
                    _ => None,
                })
                .expect("struct not found")
        }

        fn fn_decl(&self, name: &str) -> Rc<HirFn> {
            self.find_by_name(name)
                .and_then(|kind| match kind {
                    HirDeclKind::Fn(f) => Some(f.clone()),
                    _ => None,
                })
                .expect("function not found")
        }

        fn interface_decl(&self, name: &str) -> Rc<HirInterface> {
            self.find_by_name(name)
                .and_then(|kind| match kind {
                    HirDeclKind::Interface(i) => Some(i.clone()),
                    _ => None,
                })
                .expect("interface not found")
        }

        fn enum_decl(&self, name: &str) -> Rc<HirEnum> {
            self.find_by_name(name)
                .and_then(|kind| match kind {
                    HirDeclKind::Enum(e) => Some(e.clone()),
                    _ => None,
                })
                .expect("enum not found")
        }

        fn implement_decl(&self) -> Rc<HirImplement> {
            self.module
                .decls
                .iter()
                .find_map(|decl| match &decl.kind {
                    HirDeclKind::Implement(i) => Some(i.clone()),
                    _ => None,
                })
                .expect("no implement decl in module")
        }

        fn global_var(&self, name: &str) -> (Spur, Rc<HirTypeExpr>, Rc<HirExpr>, bool, bool) {
            self.find_by_name(name)
                .and_then(|kind| match kind {
                    HirDeclKind::GlobalVar {
                        name,
                        ty,
                        value,
                        is_const,
                        is_pub,
                    } => Some((name.0, ty.clone(), value.clone(), *is_const, *is_pub)),
                    _ => None,
                })
                .expect("global var not found")
        }

        fn find_by_name(&self, name: &str) -> Option<&HirDeclKind> {
            self.module.decls.iter().find_map(|decl| {
                let matches = match &decl.kind {
                    HirDeclKind::Fn(f) => self.name(f.name.0) == name,
                    HirDeclKind::Struct(s) => self.name(s.name.0) == name,
                    HirDeclKind::Interface(i) => self.name(i.name.0) == name,
                    HirDeclKind::Enum(e) => self.name(e.name.0) == name,
                    HirDeclKind::GlobalVar { name: gname, .. } => self.name(gname.0) == name,
                    _ => false,
                };

                matches.then_some(&decl.kind)
            })
        }
    }

    fn lower_full(src: &str) -> Result<Fixture, Vec<String>> {
        let rodeo = Rc::new(RefCell::new(Rodeo::default()));
        let bump = Bump::default();
        let content = Arc::new(src.to_string());
        let filename = Rc::new("test.zn".to_string());

        let mut context = CompilationContext {
            paths: PathsConfig {
                project_root: PathBuf::from("/"),
                std_root: None,
                linked: HashSet::new(),
            },
            core_files: vec![("core.ops", CORE_OPS)],
            mode: CompilationMode::Debug,
            output: CompilationOutput::EmitMIR,
            target: None,
        };

        let mut tokens = zeen_lexer::tokenize(&content);
        let mut parser = Parser::new(
            Rc::clone(&filename),
            Arc::clone(&content),
            &mut tokens,
            &bump,
            Rc::clone(&rodeo),
        );
        let program = parser
            .parse_program()
            .map_err(|errs| errs.iter().map(|e| e.to_string()).collect::<Vec<_>>())?;

        let lookup_rodeo = Rc::clone(&rodeo);

        let (resolved_program, resolution) = zeen_resolve::resolve(
            Rc::clone(&filename),
            Arc::clone(&content),
            Path::new("/test.zn"),
            program,
            &bump,
            rodeo,
            &mut context,
        )
        .map_err(|errs| errs.iter().map(|e| e.to_string()).collect::<Vec<_>>())?;

        let mut hir_lowering = HirLowering::new(&resolution, Rc::clone(&lookup_rodeo));
        let module = hir_lowering.lower_module(resolved_program);

        Ok(Fixture {
            rodeo: lookup_rodeo,
            module,
        })
    }

    fn lower_ok(src: &str) -> Fixture {
        lower_full(src).unwrap_or_else(|errors| {
            panic!(
                "expected lowering to succeed, got errors:\n{}",
                errors.join("\n")
            )
        })
    }

    #[test]
    fn struct_lowers_with_fields_types_and_names() {
        let fx = lower_ok("struct Foo { x: i32, y: bool }");
        let foo = fx.struct_decl("Foo");

        assert_eq!(foo.fields.len(), 2);
        assert_eq!(fx.name(foo.fields[0].name), "x");
        assert_eq!(fx.name(foo.fields[1].name), "y");

        assert!(matches!(
            foo.fields[0].ty.kind,
            HirTypeKind::Builtin(BuiltinType::i32)
        ));
        assert!(matches!(
            foo.fields[1].ty.kind,
            HirTypeKind::Builtin(BuiltinType::bool)
        ));

        assert_ne!(foo.fields[0].def_id, DefId(u32::MAX));
        assert_ne!(foo.fields[1].def_id, DefId(u32::MAX));
    }

    #[test]
    fn visibility_flag_is_propagated_to_struct() {
        let fx = lower_ok("pub struct Foo { x: i32 } struct Bar { y: i32 }");

        assert!(fx.struct_decl("Foo").is_pub);
        assert!(!fx.struct_decl("Bar").is_pub);
    }

    #[test]
    fn struct_method_lowers_as_fn_with_self() {
        let fx = lower_ok("struct Foo { x: i32, fn get(self) i32 { return self.x; } }");
        let foo = fx.struct_decl("Foo");

        assert_eq!(foo.methods.len(), 1);

        let method = &foo.methods[0];
        assert!(matches!(method.kind, HirDeclKind::Fn(_)));

        let HirDeclKind::Fn(f) = &method.kind else {
            unreachable!("kind already matched")
        };

        assert!(f.self_param.is_some());
        assert!(matches!(
            f.return_type.as_ref().map(|ty| &ty.kind),
            Some(HirTypeKind::Builtin(BuiltinType::i32))
        ));

        let body = f.body.as_ref().expect("method body must be lowered");
        let HirStmtKind::Expr(expr) = &body.kind else {
            panic!("method body must be an expression block")
        };
        let HirExprKind::Block { stmts, trailing } = &expr.kind else {
            panic!("method body must be a block expression")
        };

        assert!(trailing.is_none());
        assert_eq!(stmts.len(), 1);
        assert!(matches!(stmts[0].kind, HirStmtKind::Return { .. }));
    }

    #[test]
    fn fn_lowers_params_return_and_body() {
        let fx = lower_ok("fn add(a: i32, b: i32) i32 { return a + b; }");
        let add = fx.fn_decl("add");

        assert_eq!(add.params.len(), 2);
        assert!(add.params[0].def_id.is_some());
        assert!(add.params[1].def_id.is_some());
        assert!(matches!(
            add.params[0].ty.kind,
            HirTypeKind::Builtin(BuiltinType::i32)
        ));
        assert!(matches!(
            add.return_type.as_ref().map(|ty| &ty.kind),
            Some(HirTypeKind::Builtin(BuiltinType::i32))
        ));

        let body = add.body.as_ref().expect("body must be lowered");
        let HirStmtKind::Expr(expr) = &body.kind else {
            panic!("function body must be an expression block");
        };
        let HirExprKind::Block { stmts, trailing } = &expr.kind else {
            panic!("function body must be a block expression");
        };

        assert!(trailing.is_none());
        assert_eq!(stmts.len(), 1);

        let HirStmtKind::Return { value } = &stmts[0].kind else {
            panic!("block must contain a return statement");
        };

        let value = value.as_ref().expect("return value required");
        let HirExprKind::Binary { op, lhs, rhs } = &value.kind else {
            panic!("return value must be a binary expression");
        };

        assert_eq!(*op, BinaryOp::Add);

        let HirExprKind::VarRef(lhs_id) = lhs.kind else {
            panic!("add lhs must reference a parameter")
        };
        let HirExprKind::VarRef(rhs_id) = rhs.kind else {
            panic!("add rhs must reference a parameter")
        };

        assert_eq!(Some(lhs_id), add.params[0].def_id);
        assert_eq!(Some(rhs_id), add.params[1].def_id);
    }

    #[test]
    fn interface_lowers_with_methods() {
        let fx = lower_ok("interface Named { fn name(self) *char; }");
        let named = fx.interface_decl("Named");

        assert!(!named.is_pub);
        assert_eq!(named.methods.len(), 1);

        let method = &named.methods[0];
        let HirDeclKind::Fn(f) = &method.kind else {
            panic!("interface method must lower to a function")
        };

        assert!(f.body.is_none());
        assert_eq!(f.params.len(), 1);
    }

    #[test]
    fn enum_lowers_with_variants() {
        let fx = lower_ok("enum Color { Red, Green, Blue }");
        let color = fx.enum_decl("Color");

        assert_eq!(color.variants.len(), 3);
        assert_eq!(fx.name(color.variants[0].name), "Red");
        assert_eq!(fx.name(color.variants[1].name), "Green");
        assert_eq!(fx.name(color.variants[2].name), "Blue");

        for variant in &color.variants {
            assert_ne!(variant.def_id, DefId(u32::MAX));
        }
    }

    #[test]
    fn implement_lowers_interface_and_object() {
        let fx = lower_ok(
            "interface Pretty { fn pretty(self) i32; } \
             struct Foo { value: i32 } \
             implement Pretty : Foo { fn pretty(self) i32 { return self.value; } }",
        );
        let implement = fx.implement_decl();

        assert!(implement.interface.is_some());
        assert!(implement.object.is_some());
        assert_eq!(implement.methods.len(), 1);
    }

    #[test]
    fn global_var_lowers_with_type_and_value() {
        let fx = lower_ok("let g: i32 = 0; pub const c: bool = true;");

        let (g_name, g_ty, g_value, g_const, g_pub) = fx.global_var("g");
        assert_eq!(fx.name(g_name), "g");
        assert!(matches!(g_ty.kind, HirTypeKind::Builtin(BuiltinType::i32)));
        assert!(matches!(
            g_value.kind,
            HirExprKind::Literal(zeen_ast::expressions::Literal::Int(0))
        ));
        assert!(!g_const);
        assert!(!g_pub);

        let (c_name, c_ty, c_value, c_const, c_pub) = fx.global_var("c");
        assert_eq!(fx.name(c_name), "c");
        assert!(matches!(c_ty.kind, HirTypeKind::Builtin(BuiltinType::bool)));
        assert!(matches!(
            c_value.kind,
            HirExprKind::Literal(zeen_ast::expressions::Literal::Bool(true))
        ));
        assert!(c_const);
        assert!(c_pub);
    }

    #[test]
    fn global_var_reference_lowers_to_var_ref() {
        let fx = lower_ok("let a: i32 = b; let b: i32 = 5;");

        let (_, _, a_value, _, _) = fx.global_var("a");

        let HirExprKind::VarRef(b_id) = a_value.kind else {
            panic!("global var initializer must lower to a VarRef")
        };

        let b_def = fx
            .module
            .decls
            .iter()
            .find(|decl| match &decl.kind {
                HirDeclKind::GlobalVar { name, .. } => fx.name(name.0) == "b",
                _ => false,
            })
            .map(|decl| decl.def_id)
            .expect("global var b missing");

        assert_eq!(b_id, b_def);
    }

    #[test]
    fn global_var_binary_initializer_lowers_operands() {
        let fx = lower_ok("let g: i32 = 1 + 2;");

        let (_, _, value, _, _) = fx.global_var("g");

        let HirExprKind::Binary {
            ref lhs, ref rhs, ..
        } = value.kind
        else {
            panic!("initializer must lower to a Binary expression")
        };

        assert!(matches!(
            lhs.kind,
            HirExprKind::Literal(zeen_ast::expressions::Literal::Int(1))
        ));
        assert!(matches!(
            rhs.kind,
            HirExprKind::Literal(zeen_ast::expressions::Literal::Int(2))
        ));
    }

    // --> Closures

    fn closure_expr_of(fx: &Fixture, fn_name: &str) -> Rc<crate::expr::HirExpr> {
        let f = fx.fn_decl(fn_name);
        let body = f.body.as_ref().expect("body must be lowered");
        let HirStmtKind::Expr(expr) = &body.kind else {
            panic!("function body must be an expression block")
        };
        let HirExprKind::Block { stmts, .. } = &expr.kind else {
            panic!("function body must be a block expression")
        };

        stmts
            .iter()
            .find_map(|stmt| match &stmt.kind {
                HirStmtKind::Let {
                    value: Some(value), ..
                } => matches!(value.kind, HirExprKind::Closure { .. }).then(|| value.clone()),
                _ => None,
            })
            .expect("block must contain a closure let binding")
    }

    #[test]
    fn closure_lowers_to_closure_expr_with_fn() {
        let fx = lower_ok("fn main() { let c = fn(x: i32) i32 { return x + 1; }; }");

        let value = closure_expr_of(&fx, "main");

        let HirExprKind::Closure { def_id, def } = &value.kind else {
            panic!("closure value must lower to HirExprKind::Closure")
        };

        assert_ne!(*def_id, DefId(u32::MAX));
        assert!(def.parent_fn.is_some());
        assert!(!def.is_extern);
        assert!(!def.is_pub);

        assert_eq!(def.params.len(), 1);
        assert!(def.params[0].def_id.is_some());
        assert!(matches!(
            def.params[0].ty.kind,
            HirTypeKind::Builtin(BuiltinType::i32)
        ));

        assert!(matches!(
            def.return_type.as_ref().map(|ty| &ty.kind),
            Some(HirTypeKind::Builtin(BuiltinType::i32))
        ));

        let body = def.body.as_ref().expect("closure body must be lowered");
        let HirStmtKind::Expr(block) = &body.kind else {
            panic!("closure body must be an expression block")
        };
        assert!(matches!(block.kind, HirExprKind::Block { .. }));
    }

    #[test]
    fn closure_fn_has_parent_and_capture_free_param_names() {
        let fx =
            lower_ok("fn outer() void { let a = 1; let c = fn(b: i32) i32 { return a + b; }; }");

        let value = closure_expr_of(&fx, "outer");

        let HirExprKind::Closure { def, .. } = &value.kind else {
            panic!("closure value must lower to HirExprKind::Closure")
        };

        assert_eq!(fx.name(def.name.0), "closure0");
        assert_eq!(def.params.len(), 1);
        assert_eq!(
            fx.name(def.params[0].name.expect("param must be named")),
            "b"
        );
    }

    #[test]
    fn nested_closure_lowers_inside_outer_closure() {
        let fx = lower_ok(
            "fn main() { let x = 1; let outer = fn() i32 { let y = 2; let inner = fn() i32 { return x + y; }; return inner(); }; }",
        );

        let outer = closure_expr_of(&fx, "main");

        let HirExprKind::Closure {
            def_id: outer_id,
            def: outer_def,
        } = &outer.kind
        else {
            panic!("outer value must be a closure")
        };

        assert_eq!(fx.name(outer_def.name.0), "closure0");

        let outer_body = outer_def.body.as_ref().expect("outer body must be lowered");
        let HirStmtKind::Expr(outer_block) = &outer_body.kind else {
            panic!("outer body must be a block expression")
        };
        let HirExprKind::Block {
            stmts: outer_stmts, ..
        } = &outer_block.kind
        else {
            panic!("outer body must be a block")
        };

        let inner_value = match &outer_stmts[1].kind {
            HirStmtKind::Let { value, .. } => value.clone().expect("inner must have a value"),
            other => panic!("expected let binding, got {other:?}"),
        };

        let HirExprKind::Closure { def: inner_def, .. } = &inner_value.kind else {
            panic!("inner value must be a closure")
        };

        assert_eq!(fx.name(inner_def.name.0), "closure1");
        assert_eq!(inner_def.parent_fn, Some(*outer_id));
    }
}
