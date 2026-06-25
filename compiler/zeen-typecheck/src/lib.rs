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
        match &decl.kind {
            HirDeclKind::Fn(hir_fn) => {
                self.declare_fn_signature(decl.def_id, hir_fn);
            }

            HirDeclKind::Struct(s) => {
                let mut fields = Vec::with_capacity(s.fields.len());
                
                for field in &s.fields {
                    let (ty, is_const) = self.lower_hir_type_with_const(&field.ty);

                    self.result.def_types.insert(field.def_id, ty);
                    self.result.const_bindings.insert(field.def_id, is_const);

                    fields.push((field.name, field.def_id, ty));
                }

                self.result.struct_info.insert(
                    decl.def_id,
                    StructTypeInfo {
                        def_id: decl.def_id,
                        fields,
                        capabalities: Capabilities::MOVE_ONLY, // currently placeholder, resolved in phase 2
                    }
                );

                for method in &s.methods {
                    self.declare_signature(method);
                }
            }

            _ => todo!()
        };
    }

    fn declare_fn_signature(&mut self, def_id: DefId, hir_fn: &HirFn) {
        let generics: Vec<DefId> = hir_fn.generics.iter().map(|generic| generic.def_id).collect();

        let params: Vec<TypeId> = hir_fn
            .params
            .iter()
            .map(|param| {
                let (ty, is_const) = self.lower_hir_type_with_const(&param.ty);

                if let Some(param_def) = param.def_id {
                    self.result.def_types.insert(param_def, ty);
                    self.result.const_bindings.insert(param_def, is_const);
                }

                ty
            })
            .collect();

        let ret = hir_fn
            .return_type
            .as_ref()
            .map(|ty| self.lower_hir_type(ty))
            .unwrap_or_else(|| self.result.interner.void());

        let fn_ty = self.result.interner.intern(Type::Fn {
            params: params.clone(),
            ret,
        });
        self.result.def_types.insert(def_id, fn_ty);

        self.fn_sigs.insert(def_id, FnSignature { params, ret, generics });
    }

    fn lower_hir_type(&mut self, ty: &HirTypeExpr) -> TypeId {
        self.lower_hir_type_with_const(ty).0
    }

    fn lower_hir_type_with_const(&mut self, ty: &HirTypeExpr) -> (TypeId, bool) {
        if let HirTypeKind::Const(inner) = &ty.kind {
            return (self.lower_hir_type(inner), true);
        }

        (self.lower_hir_type_inner(ty), false)
    }

    fn lower_hir_type_inner(&mut self, ty: &HirTypeExpr) -> TypeId {
        match &ty.kind {
            HirTypeKind::Builtin(builtin) => self.result.interner.builtin(*builtin),

            HirTypeKind::SelfType(def_id) | HirTypeKind::SelfAlias(def_id) => {
                self.result.interner.intern(Type::Struct {
                    def_id: *def_id,
                    generic_args: Vec::new(),
                })
            },

            HirTypeKind::VaArgs => self.result.interner.void(),

            HirTypeKind::Named { def_id, generic_args } => {
                let args: Vec<TypeId> =
                    generic_args.iter().map(|ty| self.lower_hir_type(ty)).collect();

                match self.def_kind(*def_id) {
                    Some(DefKind::GenericParam) => {
                        self.result.interner.intern(Type::GenericParam(*def_id))
                    }

                    Some(DefKind::Interface) => {
                        self.result.interner.intern(Type::Interface { def_id: *def_id })
                    }

                    Some(DefKind::Enum) => {
                        self.result.interner.intern(Type::Enum { def_id: *def_id })
                    }

                    _ => self.result.interner.intern(Type::Struct {
                        def_id: *def_id,
                        generic_args: args,
                    })
                }
            }

            HirTypeKind::Const(inner) => {
                self.lower_hir_type(inner)
            }

            HirTypeKind::Pointer(inner) => {
                let is_const = matches!(inner.kind, HirTypeKind::Const(_));
                let inner_ty = self.lower_hir_type(inner);
                self.result.interner.intern(Type::Pointer { inner: inner_ty, is_const })
            }

            HirTypeKind::Array { element, len } => {
                let elem_ty = self.lower_hir_type(element);
                let len_val = len.as_ref().and_then(|expr| self.eval_const_u64(expr));
                self.result.interner.intern(Type::Array { element: elem_ty, len: len_val })
            },

            HirTypeKind::Fn { params, ret, .. } => {
                let params_tys: Vec<TypeId> =
                    params.iter().map(|param| self.lower_hir_type(param)).collect();
                let ret_ty = self.lower_hir_type(ret);

                self.result.interner.intern(Type::Fn { params: params_tys, ret: ret_ty })
            },

            HirTypeKind::Error => self.result.interner.error()
        }
    }

    fn eval_const_u64(&mut self, expr: &HirExpr) -> Option<u64> {
        match &expr.kind {
            HirExprKind::Literal(Literal::Int(n)) if *n >= 0 => Some(*n as u64),
            
            HirExprKind::Binary { lhs, rhs, op } => {
                let lhs_u64 = self.eval_const_u64(lhs)?;
                let rhs_u64 = self.eval_const_u64(rhs)?;

                let result = match op {
                    BinaryOp::Add => lhs_u64 + rhs_u64,
                    BinaryOp::Sub => {
                        if rhs_u64 > lhs_u64 || lhs_u64 - rhs_u64 == 0 {
                            self.report(TypeError::ArrayLengthNotConst {
                                src: expr.source.src(),
                                span: expr.source.span,
                            });
                            return None;
                        }

                        lhs_u64 - rhs_u64
                    },
                    BinaryOp::Mul => {
                        if lhs_u64 == 0 || rhs_u64 == 0 {
                            self.report(TypeError::ArrayLengthNotConst {
                                src: expr.source.src(),
                                span: expr.source.span,
                            });
                            return None;
                        }

                        lhs_u64 * rhs_u64
                    },
                    BinaryOp::Div => {
                        if rhs_u64 == 0 {
                            self.report(TypeError::ArrayLengthNotConst {
                                src: expr.source.src(),
                                span: expr.source.span,
                            });
                            return None;
                        }

                        lhs_u64 / rhs_u64
                    },
                    BinaryOp::Mod => {
                        if rhs_u64 == 0 {
                            self.report(TypeError::ArrayLengthNotConst {
                                src: expr.source.src(),
                                span: expr.source.span,
                            });
                            return None;
                        }

                        lhs_u64 % rhs_u64
                    },

                    _ => {
                        self.report(TypeError::ArrayLengthNotConst {
                            src: expr.source.src(),
                            span: expr.source.span,
                        });
                        return None;
                    }
                };
                
                Some(result)
            }

            _ => {
                self.report(TypeError::ArrayLengthNotConst {
                    src: expr.source.src(),
                    span: expr.source.span,
                });
                None
            }
        }
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
