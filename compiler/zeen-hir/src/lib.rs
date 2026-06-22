#![allow(unused)]

use std::{
    rc::Rc,
    sync::{Arc, Mutex},
};

use lasso::{Rodeo, Spur};
use miette::SourceSpan;
use smol_str::SmolStr;

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

pub use expr::{HirExpr, HirExprKind, HirFieldInit, HirMacroKind};
pub use stmt::{HirStmt, HirStmtKind};
pub use types::{HirTypeExpr, HirTypeKind};

// =========| HIR Lowering |=========

pub struct HirLowering<'res> {
    resolution: &'res ResolutionResult,
    interner: Arc<Mutex<Rodeo>>,

    next_id: u32,
    current_src: miette::NamedSource<Arc<String>>,
}

impl<'res> HirLowering<'res> {
    pub fn new(resolution: &'res ResolutionResult, interner: Arc<Mutex<Rodeo>>) -> Self {
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

    fn interner_intern(&mut self, value: impl AsRef<str>) -> lasso::Spur {
        // compiler is not async/threaded (at least for now), so we're unwrapping lock
        let mut interner = self.interner.lock().unwrap();

        interner.get_or_intern(value)
    }

    fn interner_resolve(&self, key: &Spur) -> SmolStr {
        let interner = self.interner.lock().unwrap();
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
            _ => None,
        }
    }

    fn resolve_macro_kind(&self, name: Spur) -> HirMacroKind {
        let name = self.interner_resolve(&name);

        match name.as_str() {
            "as" => HirMacroKind::As,
            "sizeof" => HirMacroKind::SizeOf,
            "alignof" => HirMacroKind::AlignOf,

            "print" => HirMacroKind::Print,
            "println" => HirMacroKind::Println,
            "format" => HirMacroKind::Format,

            "panic" => HirMacroKind::Panic,
            "unreachable" => HirMacroKind::Unreachable,
            "dbg" => HirMacroKind::Dbg,

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
                interface,
                object,
                methods,
                generics,
            } => {
                let object_def = match self.resolution.resolution_of_type(&TypeExpr {
                    kind: TypeKind::Named {
                        name: object.0,
                        generic_args: None,
                    },
                    span: object.1,
                }) {
                    Some(res) => {
                        if let Resolution::Def(id) = res {
                            Some(id)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };

                let interface_def = match self.resolution.resolution_of_type(&TypeExpr {
                    kind: TypeKind::Named {
                        name: interface.0,
                        generic_args: None,
                    },
                    span: interface.1,
                }) {
                    Some(res) => {
                        if let Resolution::Def(id) = res {
                            Some(id)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };

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

            StatementKind::Defer { body } => HirStmtKind::Defer {
                body: Rc::new(self.lower_stmt(body)),
            },

            StatementKind::Break => HirStmtKind::Break,

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

            ExpressionKind::MacroCall { name, args } => {
                HirExprKind::MacroCall {
                    kind: (self.resolve_macro_kind(name.0), name.1),
                    args: args.iter().map(|arg| Rc::new(self.lower_expr(arg))).collect()
                }
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

                HirExprKind::StructInit {
                    ty: (ty_def, ty.span),
                    fields: hir_fields,
                }
            }

            ExpressionKind::ArrayInit { elements } => HirExprKind::ArrayInit {
                elements: elements
                    .iter()
                    .map(|e| Rc::new(self.lower_expr(e)))
                    .collect(),
            },

            ExpressionKind::Block(stmts) => HirExprKind::Block(
                stmts
                    .iter()
                    .map(|stmt| Rc::new(self.lower_stmt(stmt)))
                    .collect(),
            ),

            ExpressionKind::Type(ty) => HirExprKind::Type(Rc::new(self.lower_type(ty))),
        };

        HirExpr {
            id: self.fresh_id(),
            kind,
            source: (expr.span, self.current_src.clone()).into(),
        }
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
            TypeKind::Pointer(inner) => HirTypeKind::Pointer(Rc::new(self.lower_type(inner))),

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
        };

        HirTypeExpr {
            id: self.fresh_id(),
            kind,
            source: (ty.span, self.current_src.clone()).into(),
        }
    }
}
