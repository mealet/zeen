#![allow(unused)]

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use lasso::Spur;
use miette::SourceSpan;
use zeen_ast::Source;

use crate::error::TypeError;
use crate::{
    coerce::{CoerceResult, try_coerce},
    context::{FnCtx, TypeCheckCtx},
    result::{CallResolution, TypeCheckResult},
    types::{Capabilities, StructTypeInfo, Type, TypeId, TypeInterner},
};

use zeen_ast::{
    expressions::{BinaryOp, Literal, UnaryOp},
    types::BuiltinType,
};
use zeen_hir::{
    HirId, HirModule,
    decl::{HirDecl, HirDeclKind, HirFn},
    expr::{HirExpr, HirExprKind, HirFieldInit, HirMacroKind},
    stmt::{HirStmt, HirStmtKind},
    types::{HirTypeExpr, HirTypeKind},
};
use zeen_resolve::{DefId, DefKind, ResolutionResult};

mod coerce;
mod context;
mod error;
mod result;
mod types;

pub const DEFAULT_INT_LITERAL: BuiltinType = BuiltinType::i32;
pub const DEFAULT_FLOAT_LITERAL: BuiltinType = BuiltinType::f64;

pub struct TypeChecker<'res> {
    resolution: &'res ResolutionResult,

    result: TypeCheckResult,
    ctx: TypeCheckCtx,
    interner: Rc<RefCell<lasso::Rodeo>>,

    fn_sigs: HashMap<DefId, FnSignature>,
}

struct FnSignature {
    params: Vec<TypeId>,
    ret: TypeId,
    generics: Vec<DefId>,
}

impl<'res> TypeChecker<'res> {
    pub fn new(resolution: &'res ResolutionResult, interner: Rc<RefCell<lasso::Rodeo>>) -> Self {
        Self {
            resolution,
            result: TypeCheckResult::default(),
            ctx: TypeCheckCtx::new(),
            interner,
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

    fn display_type(&self, id: TypeId) -> String {
        self.result
            .interner
            .display_type(id, Rc::clone(&self.interner), self.resolution)
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
                    },
                );

                for method in &s.methods {
                    self.declare_signature(method);
                }
            }

            HirDeclKind::Interface(i) => {
                for method in &i.methods {
                    self.declare_signature(method);
                }
            }

            HirDeclKind::Implement(imp) => {
                for method in &imp.methods {
                    self.declare_signature(method);
                }
            }

            HirDeclKind::Enum(e) => {
                let enum_ty = self.result.interner.intern(Type::Enum {
                    def_id: decl.def_id,
                });

                for variant in &e.variants {
                    self.result.def_types.insert(variant.def_id, enum_ty);
                }
            }

            HirDeclKind::ExternVar { ty, .. } => {
                let ty_id = self.lower_hir_type(ty);
                self.result.def_types.insert(decl.def_id, ty_id);
            }

            HirDeclKind::ExternLink | HirDeclKind::ExternInclude => {}
        };
    }

    fn declare_fn_signature(&mut self, def_id: DefId, hir_fn: &HirFn) {
        let generics: Vec<DefId> = hir_fn
            .generics
            .iter()
            .map(|generic| generic.def_id)
            .collect();

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

        self.fn_sigs.insert(
            def_id,
            FnSignature {
                params,
                ret,
                generics,
            },
        );
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
            }

            HirTypeKind::VaArgs => self.result.interner.void(),

            HirTypeKind::Named {
                def_id,
                generic_args,
            } => {
                let args: Vec<TypeId> = generic_args
                    .iter()
                    .map(|ty| self.lower_hir_type(ty))
                    .collect();

                match self.def_kind(*def_id) {
                    Some(DefKind::GenericParam) => {
                        self.result.interner.intern(Type::GenericParam(*def_id))
                    }

                    Some(DefKind::Interface) => self
                        .result
                        .interner
                        .intern(Type::Interface { def_id: *def_id }),

                    Some(DefKind::Enum) => {
                        self.result.interner.intern(Type::Enum { def_id: *def_id })
                    }

                    _ => self.result.interner.intern(Type::Struct {
                        def_id: *def_id,
                        generic_args: args,
                    }),
                }
            }

            HirTypeKind::Const(inner) => self.lower_hir_type(inner),

            HirTypeKind::Pointer(inner) => {
                let is_const = matches!(inner.kind, HirTypeKind::Const(_));
                let inner_ty = self.lower_hir_type(inner);
                self.result.interner.intern(Type::Pointer {
                    inner: inner_ty,
                    is_const,
                })
            }

            HirTypeKind::Array { element, len } => {
                let elem_ty = self.lower_hir_type(element);
                let len_val = len.as_ref().and_then(|expr| self.eval_const_u64(expr));
                self.result.interner.intern(Type::Array {
                    element: elem_ty,
                    len: len_val,
                })
            }

            HirTypeKind::Fn { params, ret, .. } => {
                let params_tys: Vec<TypeId> = params
                    .iter()
                    .map(|param| self.lower_hir_type(param))
                    .collect();
                let ret_ty = self.lower_hir_type(ret);

                self.result.interner.intern(Type::Fn {
                    params: params_tys,
                    ret: ret_ty,
                })
            }

            HirTypeKind::Error => self.result.interner.error(),
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
                    }
                    BinaryOp::Mul => {
                        if lhs_u64 == 0 || rhs_u64 == 0 {
                            self.report(TypeError::ArrayLengthNotConst {
                                src: expr.source.src(),
                                span: expr.source.span,
                            });
                            return None;
                        }

                        lhs_u64 * rhs_u64
                    }
                    BinaryOp::Div => {
                        if rhs_u64 == 0 {
                            self.report(TypeError::ArrayLengthNotConst {
                                src: expr.source.src(),
                                span: expr.source.span,
                            });
                            return None;
                        }

                        lhs_u64 / rhs_u64
                    }
                    BinaryOp::Mod => {
                        if rhs_u64 == 0 {
                            self.report(TypeError::ArrayLengthNotConst {
                                src: expr.source.src(),
                                span: expr.source.span,
                            });
                            return None;
                        }

                        lhs_u64 % rhs_u64
                    }

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
        if let HirDeclKind::Struct(_) = &decl.kind {
            let mut visiting = Vec::new();
            self.compute_capabilities(decl.def_id, &mut visiting);
        }
    }

    fn compute_capabilities(&mut self, def_id: DefId, visiting: &mut Vec<DefId>) -> Capabilities {
        if visiting.contains(&def_id) {
            return Capabilities::MOVE_ONLY;
        }
        visiting.push(def_id);

        let field_types: Vec<TypeId> = self
            .result
            .struct_info
            .get(&def_id)
            .map(|info| info.fields.iter().map(|(_, _, ty)| *ty).collect())
            .unwrap_or_default();

        let mut is_copy = true;
        let mut needs_drop = false;

        for field_ty in field_types {
            let caps = self.capabilities_of_type(field_ty, visiting);

            is_copy &= caps.is_copy;
            needs_drop |= caps.needs_drop;
        }

        visiting.pop();

        let caps = Capabilities {
            is_copy,
            needs_drop,
        };

        if let Some(info) = self.result.struct_info.get_mut(&def_id) {
            info.capabalities = caps;
        }

        caps
    }

    fn capabilities_of_type(&mut self, ty: TypeId, visiting: &mut Vec<DefId>) -> Capabilities {
        match self.result.interner.get(ty) {
            Type::Builtin(_)
            | Type::IntLiteral
            | Type::FloatLiteral
            | Type::Interface { .. }
            | Type::Enum { .. }
            | Type::Pointer { .. }
            | Type::Fn { .. }
            | Type::GenericParam { .. }
            | Type::Void
            | Type::Never
            | Type::Error => Capabilities::COPY,

            Type::Struct { def_id, .. } => self.compute_capabilities(*def_id, visiting),

            Type::Array { element, .. } | Type::Slice { element, .. } => {
                self.capabilities_of_type(*element, visiting)
            }
        }
    }

    // > Pass 3

    // Declarations

    fn check_decl_body(&mut self, decl: &HirDecl) {
        match &decl.kind {
            HirDeclKind::Fn(hir_fn) => self.check_fn_body(decl.def_id, hir_fn, None),

            HirDeclKind::Struct(s) => {
                let self_ty = self.result.interner.intern(Type::Struct {
                    def_id: decl.def_id,
                    generic_args: Vec::new(),
                });

                for method in &s.methods {
                    self.check_decl_body_as_method(method, self_ty);
                }
            }

            HirDeclKind::Interface(i) => {
                for method in &i.methods {
                    if let HirDeclKind::Fn(f) = &method.kind {
                        self.check_fn_body(method.def_id, f, None);
                    }
                }
            }

            HirDeclKind::Implement(imp) => {
                if let Some(object_def) = imp.object {
                    let self_ty = self.result.interner.intern(Type::Struct {
                        def_id: object_def,
                        generic_args: Vec::new(),
                    });

                    for method in &imp.methods {
                        self.check_decl_body_as_method(method, self_ty);
                    }
                } else {
                    for method in &imp.methods {
                        if let HirDeclKind::Fn(f) = &method.kind {
                            self.check_fn_body(method.def_id, f, None);
                        }
                    }
                }
            }

            HirDeclKind::Enum(_)
            | HirDeclKind::ExternVar { .. }
            | HirDeclKind::ExternLink
            | HirDeclKind::ExternInclude => {}
        }
    }

    fn check_decl_body_as_method(&mut self, method: &HirDecl, self_ty: TypeId) {
        if let HirDeclKind::Fn(f) = &method.kind {
            self.check_fn_body(method.def_id, f, Some(self_ty));
        }
    }

    fn check_fn_body(&mut self, def_id: DefId, hir_fn: &HirFn, self_ty: Option<TypeId>) {
        let Some(body) = &hir_fn.body else {
            return;
        };

        let sig = self
            .fn_sigs
            .get(&def_id)
            .expect("unregistered signature, wtf");

        let mut generic_bindings = HashMap::new();

        for generic in &sig.generics {
            let ty = self.result.interner.intern(Type::GenericParam(*generic));
            generic_bindings.insert(*generic, ty);
        }

        self.ctx.push_fn(FnCtx {
            return_type: sig.ret,
            self_type: self_ty,
            generic_bindings,
            loop_depth: 0,
        });

        self.check_stmt(body);

        self.ctx.pop_fn();
    }

    // Statements

    fn check_stmt(&mut self, stmt: &HirStmt) {
        match &stmt.kind {
            HirStmtKind::Let {
                def_id,
                name,
                explicit_type,
                value,
                is_const,
            } => {
                let declared = explicit_type
                    .as_ref()
                    .map(|t| self.lower_hir_type_with_const(t));

                let declared_ty = declared.map(|(ty, _)| ty);
                let declared_const = declared.map(|(_, c)| c).unwrap_or(false);

                let value_ty = value.as_ref().map(|val| match declared_ty {
                    Some(expected) => self.check_expr(val, expected),
                    None => self.synth_expr(val),
                });

                let final_ty = match (declared_ty, value_ty) {
                    (Some(t), _) => t,
                    (None, Some(t)) => self.default_literal(t),
                    (None, None) => self.result.interner.error(),
                };

                self.result.def_types.insert(*def_id, final_ty);

                self.result
                    .const_bindings
                    .insert(*def_id, *is_const || declared_const);
            }

            HirStmtKind::Assign { object, value } => {
                let obj_ty = self.synth_expr(object);
                self.check_expr(value, obj_ty);
                self.check_not_const_target(object);
            }

            HirStmtKind::Expr(expr) => {
                self.synth_expr(expr);
            }

            HirStmtKind::Error => {}

            stmt => todo!("{:#?}", stmt),
        }
    }

    // Expressions

    fn synth_expr(&mut self, expr: &HirExpr) -> TypeId {
        let ty = self.synth_expr_inner(expr);
        self.result.record_expr_type(expr.id, ty);
        ty
    }

    fn synth_expr_inner(&mut self, expr: &HirExpr) -> TypeId {
        match &expr.kind {
            HirExprKind::Literal(lit) => match lit {
                Literal::Int(_) => self.result.interner.int_literal(),
                Literal::Float(_) => self.result.interner.float_literal(),
                Literal::Char(_) => self.result.interner.builtin(BuiltinType::char),
                Literal::ByteChar(_) => self.result.interner.builtin(BuiltinType::u8),
                Literal::Bool(_) => self.result.interner.builtin(BuiltinType::bool),
                Literal::String(_) => {
                    let char_ty = self.result.interner.builtin(BuiltinType::char);
                    self.result.interner.intern(Type::Pointer {
                        inner: char_ty,
                        is_const: true,
                    })
                }
                Literal::Null => {
                    let void = self.result.interner.void();
                    self.result.interner.intern(Type::Pointer {
                        inner: void,
                        is_const: false,
                    })
                }
            },

            HirExprKind::VarRef(def_id) => self.lookup_def_type(*def_id, expr.source.clone()),

            HirExprKind::GenericParamRef(def_id) => self
                .ctx
                .generic_binding(*def_id)
                .unwrap_or(self.result.interner.intern(Type::GenericParam(*def_id))),

            HirExprKind::SelfValue(def_id) => self
                .ctx
                .generic_binding(*def_id)
                .unwrap_or(self.lookup_def_type(*def_id, expr.source.clone())),

            HirExprKind::MacroCall { kind, args } => {
                self.check_macro_call(*kind, args, expr.source.clone())
            }

            HirExprKind::Block(stmts) => self.synth_block_value_stmts(stmts),

            HirExprKind::Type(_) => self.result.interner.error(),
            HirExprKind::Error => self.result.interner.error(),

            _ => todo!(),
        }
    }

    fn check_macro_call(
        &mut self,
        kind: (HirMacroKind, SourceSpan),
        args: &[Rc<HirExpr>],
        source: Source,
    ) -> TypeId {
        match kind.0 {
            HirMacroKind::Print | HirMacroKind::Println => {
                for arg in args {
                    self.synth_expr(arg);
                }
                self.result.interner.void()
            }

            HirMacroKind::Format => {
                for arg in args {
                    self.synth_expr(arg);
                }
                let char_ty = self.result.interner.builtin(BuiltinType::char);

                self.result.interner.intern(Type::Pointer {
                    inner: char_ty,
                    is_const: true,
                })
            }

            HirMacroKind::Panic | HirMacroKind::Unreachable => {
                for arg in args {
                    self.synth_expr(arg);
                }

                self.result.interner.never()
            }

            _ => todo!(),
        }
    }

    fn check_expr(&mut self, expr: &HirExpr, expected: TypeId) -> TypeId {
        let actual = match &expr.kind {
            HirExprKind::ArrayInit { elements } if elements.is_empty() => {
                if let Type::Array { .. } = self.result.interner.get(expected).clone() {
                    self.result.record_expr_type(expr.id, expected);
                    return expected;
                }
                self.synth_expr(expr)
            }
            _ => self.synth_expr(expr),
        };

        self.coerce_or_error(actual, expected, expr.source.clone(), expr.id)
    }

    fn coerce_or_error(
        &mut self,
        actual: TypeId,
        expected: TypeId,
        source: Source,
        id: HirId,
    ) -> TypeId {
        match try_coerce(&self.result.interner, actual, expected) {
            CoerceResult::Identity => actual,
            CoerceResult::ErrorRecovery => expected,

            CoerceResult::PinLiteral
            | CoerceResult::AddConst
            | CoerceResult::ArrayToSlice
            | CoerceResult::NeverCoercion => {
                self.result.record_expr_type(id, expected);
                expected
            }

            CoerceResult::Fail => {
                self.result.errors.push(TypeError::Mismatch {
                    expected: self.display_type(expected).into(),
                    found: self.display_type(actual).into(),
                    src: source.src(),
                    span: source.span,
                });

                actual
            }
        }
    }

    fn check_not_const_target(&mut self, target: &HirExpr) {
        if self.find_const_violation(target) {
            self.result.errors.push(TypeError::AssignToConst {
                src: target.source.src(),
                span: target.source.span,
            })
        }
    }

    fn find_const_violation(&mut self, target: &HirExpr) -> bool {
        match &target.kind {
            HirExprKind::Unary {
                expr: ptr_expr,
                op: UnaryOp::Deref,
            } => {
                if let Some(&ptr_ty) = self.result.expr_types.get(&ptr_expr.id)
                    && let Type::Pointer { is_const: true, .. } = self.result.interner.get(ptr_ty)
                {
                    return true;
                }

                false
            }

            HirExprKind::FieldAccess { object, .. } => {
                let field_const = self
                    .result
                    .field_resolutions
                    .get(&target.id)
                    .and_then(|def_id| self.result.const_bindings.get(def_id))
                    .copied()
                    .unwrap_or(false);

                field_const || self.find_const_violation(object)
            }

            HirExprKind::SliceAccess { object, .. } => self.find_const_violation(object),

            HirExprKind::VarRef(def_id) | HirExprKind::SelfValue(def_id) => self
                .result
                .const_bindings
                .get(def_id)
                .copied()
                .unwrap_or(false),

            _ => false,
        }
    }

    fn lookup_def_type(&mut self, def_id: DefId, source: Source) -> TypeId {
        self.result
            .def_types
            .get(&def_id)
            .copied()
            .unwrap_or_else(|| {
                self.report(TypeError::DanglingDefId {
                    id: def_id.0,
                    src: source.src(),
                    span: source.span,
                });
                self.result.interner.error()
            })
    }

    fn default_literal(&mut self, ty: TypeId) -> TypeId {
        match self.result.interner.get(ty) {
            Type::IntLiteral => self.result.interner.builtin(DEFAULT_INT_LITERAL),
            Type::FloatLiteral => self.result.interner.builtin(DEFAULT_FLOAT_LITERAL),
            _ => ty,
        }
    }

    fn synth_block_value(&mut self, stmt: &HirStmt) -> TypeId {
        self.check_stmt(stmt);

        match &stmt.kind {
            HirStmtKind::Expr(expr) => self
                .result
                .expr_types
                .get(&expr.id)
                .copied()
                .unwrap_or(self.result.interner.void()),

            _ => self.result.interner.void(),
        }
    }

    fn synth_block_value_stmts(&mut self, stmts: &[Rc<HirStmt>]) -> TypeId {
        for stmt in stmts {
            self.check_stmt(stmt);
        }

        match stmts.last().map(|stmt| &stmt.kind) {
            Some(HirStmtKind::Expr(expr)) => self
                .result
                .expr_types
                .get(&expr.id)
                .copied()
                .unwrap_or(self.result.interner.void()),

            _ => self.result.interner.void(),
        }
    }
}
