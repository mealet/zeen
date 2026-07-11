#![allow(unused)]

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    ops::Deref,
    rc::Rc,
};

use lasso::Spur;
use miette::SourceSpan;
use zeen_ast::Source;

use crate::{
    coerce::{CoerceResult, try_coerce},
    context::{FnCtx, TypeCheckCtx},
    format_str::FormatSpec,
    result::{CallResolution, TypeCheckResult},
    types::{Capabilities, SelfMode, StructTypeInfo, Type, TypeId, TypeInterner, self_mode_of},
};
use crate::{error::TypeError, format_str::FormatParseError};

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
mod format_str;
mod result;
mod types;

pub const DEFAULT_INT_LITERAL: BuiltinType = BuiltinType::i32;
pub const DEFAULT_FLOAT_LITERAL: BuiltinType = BuiltinType::f64;

pub struct TypeChecker<'res> {
    resolution: &'res mut ResolutionResult,
    expect_assign_interface: bool,

    result: TypeCheckResult,
    ctx: TypeCheckCtx,
    interner: Rc<RefCell<lasso::Rodeo>>,

    fn_sigs: HashMap<DefId, FnSignature>,
    struct_generics: HashMap<DefId, Vec<DefId>>,
    interface_methods: HashMap<DefId, Vec<DefId>>,
    struct_methods: HashMap<DefId, HashMap<Spur, DefId>>,
}

struct FnSignature {
    params: Vec<TypeId>,
    ret: TypeId,
    generics: Vec<DefId>,
    generic_bounds: HashMap<DefId, Vec<DefId>>,
    self_mode: Option<SelfMode>,
}

impl<'res> TypeChecker<'res> {
    pub fn new(
        resolution: &'res mut ResolutionResult,
        interner: Rc<RefCell<lasso::Rodeo>>,
    ) -> Self {
        Self {
            resolution,
            result: TypeCheckResult::default(),
            ctx: TypeCheckCtx::new(),
            interner,
            fn_sigs: HashMap::new(),
            struct_generics: HashMap::new(),
            expect_assign_interface: false,
            interface_methods: HashMap::new(),
            struct_methods: HashMap::new(),
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

    fn inject_well_known_interfaces_methods(&mut self) {
        todo!()
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
                self.struct_generics
                    .insert(decl.def_id, s.generics.iter().map(|g| g.def_id).collect());

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
                        capabalities: Capabilities::MOVE_ONLY,
                    },
                );

                for method in &s.methods {
                    self.declare_signature(method);

                    if let HirDeclKind::Fn(f) = &method.kind {
                        self.struct_methods
                            .entry(decl.def_id)
                            .or_default()
                            .insert(f.name.0, method.def_id);
                    }
                }
            }

            HirDeclKind::Interface(i) => {
                self.interface_methods
                    .insert(decl.def_id, i.methods.iter().map(|m| m.def_id).collect());

                for method in &i.methods {
                    self.declare_signature(method);
                }
            }

            HirDeclKind::Implement(imp) => {
                self.declare_implement_signature(imp, decl.source.clone());
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

        let generic_bounds: HashMap<DefId, Vec<DefId>> = hir_fn
            .generics
            .iter()
            .map(|g| (g.def_id, g.bounds.clone()))
            .collect();

        let self_mode = hir_fn.params.first().and_then(|p| self_mode_of(&p.ty.kind));

        let params: Vec<TypeId> = hir_fn
            .params
            .iter()
            .enumerate()
            .map(|(idx, param)| {
                let (ty, is_const) = self.lower_hir_type_with_const(&param.ty);

                let effective_const = if idx == 0 {
                    match self_mode_of(&param.ty.kind) {
                        Some(SelfMode::ValueConst) | Some(SelfMode::RefConst) => true,
                        _ => is_const,
                    }
                } else {
                    is_const
                };

                if let Some(param_def) = param.def_id {
                    self.result.def_types.insert(param_def, ty);
                    self.result
                        .const_bindings
                        .insert(param_def, effective_const);
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
                generic_bounds,
                self_mode,
            },
        );
    }

    fn declare_implement_signature(&mut self, imp: &zeen_hir::HirImplement, source: Source) {
        for method in &imp.methods {
            self.declare_signature(method);

            if let (HirDeclKind::Fn(f), Some(object_def)) = (&method.kind, imp.object) {
                self.struct_methods
                    .entry(object_def)
                    .or_default()
                    .insert(f.name.0, method.def_id);
            }
        }

        let Some(object_def) = imp.object else { return };

        let struct_generics = self
            .struct_generics
            .get(&object_def)
            .cloned()
            .unwrap_or_default();

        if imp.object_generics_bindings.len() != struct_generics.len() {
            let name_id = self
                .resolution
                .defs
                .get(&object_def)
                .expect("object def id is wrong")
                .name;
            let interner = self.interner.borrow();
            let name = interner.resolve(&name_id).into();

            drop(interner);

            self.report(TypeError::GenericArgCountMismatch {
                name,
                expected: struct_generics.len(),
                found: imp.object_generics_bindings.len(),
                src: source.src(),
                span: imp.object_bindings_span,
            });
        }

        let Some(iface_def) = imp.interface else {
            return;
        };

        let self_generic_args: Vec<TypeId> = struct_generics
            .iter()
            .map(|&g| self.result.interner.intern(Type::GenericParam(g)))
            .collect();

        let self_struct_ty = self.result.interner.intern(Type::Struct {
            def_id: object_def,
            generic_args: self_generic_args,
        });
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
                match self.def_kind(*def_id) {
                    Some(DefKind::InterfaceSelfPlaceholder) => self
                        .result
                        .interner
                        .intern(Type::InterfaceSelfPlaceholder(*def_id)),

                    _ => self.result.interner.intern(Type::Struct {
                        def_id: *def_id,
                        generic_args: Vec::new(),
                    }),
                }
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

            Type::InterfaceSelfPlaceholder(def_id) => self.compute_capabilities(*def_id, visiting),
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
                    self.check_decl_body_as_method(method, decl.def_id);
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
                        self.check_decl_body_as_method(method, object_def);
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

    fn check_decl_body_as_method(&mut self, method: &HirDecl, struct_def: DefId) {
        if let HirDeclKind::Fn(f) = &method.kind {
            self.check_fn_body(method.def_id, f, Some(struct_def));
        }
    }

    fn check_fn_body(&mut self, def_id: DefId, hir_fn: &HirFn, struct_def: Option<DefId>) {
        let Some(body) = &hir_fn.body else {
            return;
        };

        let sig = self
            .fn_sigs
            .get(&def_id)
            .expect("unregistered signature, wtf");

        let self_type = struct_def.map(|sd| {
            let base_struct_ty = self.result.interner.intern(Type::Struct {
                def_id: sd,
                generic_args: Vec::new(),
            });

            match sig.self_mode {
                Some(SelfMode::Value) | Some(SelfMode::ValueConst) | None => base_struct_ty,
                Some(SelfMode::RefMut) => self.result.interner.intern(Type::Pointer {
                    inner: base_struct_ty,
                    is_const: false,
                }),
                Some(SelfMode::RefConst) => self.result.interner.intern(Type::Pointer {
                    inner: base_struct_ty,
                    is_const: true,
                }),
            }
        });

        let mut generic_bindings = HashMap::new();
        let mut generic_bounds = HashMap::new();

        for generic in &sig.generics {
            let ty = self.result.interner.intern(Type::GenericParam(*generic));
            generic_bindings.insert(*generic, ty);
        }

        for (g, bounds) in &sig.generic_bounds {
            generic_bounds.insert(*g, bounds.clone());
        }

        self.ctx.push_fn(FnCtx {
            return_type: sig.ret,
            self_type,
            generic_bindings,
            generic_bounds,
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
                    Some(expected) => self.check_expr(val, expected, true),
                    None => self.synth_expr(val),
                });

                let final_ty = match (declared_ty, value_ty) {
                    (Some(t), _) => t,
                    (None, Some(t)) => self.default_literal(t),
                    (None, None) => self.result.interner.error(),
                };

                self.result.def_types.insert(*def_id, final_ty);

                self.result.const_bindings.insert(*def_id, *is_const);
            }

            HirStmtKind::Assign { object, value } => {
                let prev_expect = self.expect_assign_interface;
                self.expect_assign_interface = true;

                let obj_ty = self.synth_expr(object);
                self.expect_assign_interface = false;

                self.check_expr(value, obj_ty, false);
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
                self.check_macro_call(expr.id, *kind, args, expr.source.clone())
            }

            HirExprKind::Binary { lhs, rhs, op } => {
                let lhs_ty = self.synth_expr(lhs);
                let rhs_ty = self.synth_expr(rhs);

                self.check_binary_op(*op, lhs_ty, rhs_ty, expr.source.clone())
            }

            HirExprKind::Unary { expr, op } => {
                let inner_ty = self.synth_expr(expr);
                self.check_unary_op(*op, inner_ty, expr.source.clone())
            }

            HirExprKind::Call { callee, args, generic_args } => {
                self.check_call(expr.id, callee, args, generic_args, expr.source.clone())
            }

            HirExprKind::Block { stmts, trailing: _ } => self.synth_block_value_stmts(stmts),

            HirExprKind::Type(_) => self.result.interner.error(),
            HirExprKind::Error => self.result.interner.error(),

            _ => todo!(),
        }
    }

    // >> Macros

    fn check_macro_call(
        &mut self,
        call_id: HirId,
        kind: (HirMacroKind, SourceSpan),
        args: &[Rc<HirExpr>],
        source: Source,
    ) -> TypeId {
        match kind.0 {
            HirMacroKind::Print | HirMacroKind::Println => {
                self.check_format_macro(call_id, args, source);
                self.result.interner.void()
            }

            HirMacroKind::Format => {
                self.check_format_macro(call_id, args, source);

                let char_ty = self.result.interner.builtin(BuiltinType::char);
                self.result.interner.intern(Type::Pointer {
                    inner: char_ty,
                    is_const: true,
                })
            }

            HirMacroKind::Panic | HirMacroKind::Unreachable => {
                self.check_format_macro(call_id, args, source);
                self.result.interner.never()
            }

            HirMacroKind::Unreachable => {
                if !args.is_empty() {
                    self.report(TypeError::ArgCountMismatch {
                        expected: 0,
                        found: args.len(),
                        src: source.src(),
                        span: source.span,
                    });
                }

                self.result.interner.never()
            }

            HirMacroKind::Dbg => {
                if args.len() != 1 {
                    self.report(TypeError::ArgCountMismatch {
                        expected: 0,
                        found: args.len(),
                        src: source.src(),
                        span: source.span,
                    });

                    return self.result.interner.error();
                };

                let arg = Rc::clone(&args[0]);
                let ty = self.synth_expr(arg.as_ref());

                {
                    // FIXME: Fix this (WellKnownInterfacesMap is deprecated)

                    // self.check_implements_interface(ty, IFACE_NAME, iface_def);
                }

                ty
            }

            HirMacroKind::SizeOf | HirMacroKind::AlignOf => {
                if args.len() != 1 {
                    self.report(TypeError::ArgCountMismatch {
                        expected: 0,
                        found: args.len(),
                        src: source.src(),
                        span: source.span,
                    });

                    return self.result.interner.error();
                };

                if let Some(arg) = args.first() {
                    self.synth_expr(arg);
                }
                self.result.interner.builtin(BuiltinType::usize)
            }

            HirMacroKind::As => {
                if args.len() != 2 {
                    self.report(TypeError::ArgCountMismatch {
                        expected: 0,
                        found: args.len(),
                        src: source.src(),
                        span: source.span,
                    });

                    return self.result.interner.error();
                };

                let target_ty = match &args[0].kind {
                    HirExprKind::Type(ty_expr) => self.lower_hir_type(ty_expr),
                    _ => {
                        self.synth_expr(&args[0]);
                        self.result.interner.error()
                    }
                };
                self.synth_expr(&args[1]);

                target_ty
            }

            HirMacroKind::Unknown => {
                for arg in args {
                    self.synth_expr(arg);
                }

                self.report(TypeError::UnknownMacro {
                    src: source.src(),
                    span: kind.1,
                });
                self.result.interner.error()
            }
        }
    }

    fn check_format_macro(&mut self, call_id: HirId, args: &[Rc<HirExpr>], source: Source) {
        let Some(fmt_arg) = args.first() else {
            self.report(TypeError::ExpectedFormatString {
                src: source.src(),
                span: source.span,
            });

            return;
        };

        let fmt_str: String = match &fmt_arg.kind {
            HirExprKind::Literal(Literal::String(str_id)) => {
                let interner = self.interner.borrow();
                interner.resolve(str_id).to_string()
            }

            _ => {
                let (src, span) = (fmt_arg.source.src(), fmt_arg.source.span);
                self.report(TypeError::ExpectedFormatString { src, span });
                return;
            }
        };

        let chunks = match format_str::parse_format_string(&fmt_str) {
            Ok(chunks) => chunks,
            Err(err) => {
                self.report(format_error_to_diagnostic(&err, &fmt_arg.source));

                // still check remaining args
                for arg in &args[1..] {
                    self.synth_expr(arg);
                }

                return;
            }
        };

        self.result.format_specs.insert(call_id, chunks.clone());

        let specs = format_str::arg_specs(&chunks);
        let value_args = &args[1..];

        if specs.len() != value_args.len() {
            self.report(TypeError::FormatArgCountMismatch {
                placeholders: specs.len(),
                args: value_args.len(),
                src: fmt_arg.source.src(),
                span: fmt_arg.source.span,
            });
            // return;
        }

        for (spec, arg) in specs.iter().zip(value_args) {
            let arg_ty = self.synth_expr(arg);
            let arg_ty = self.default_literal(arg_ty);

            self.check_format_arg(*spec, arg_ty, arg.source.clone());
        }

        // checking extra args
        for arg in value_args.iter().skip(specs.len()) {
            self.synth_expr(arg);
        }
    }

    fn check_format_arg(&mut self, spec: FormatSpec, arg_ty: TypeId, source: Source) -> Option<()> {
        match spec {
            FormatSpec::Display => {
                // FIXME: Fix this (WellKnownInterfacesMap is deprecated)

                // self.check_implements_interface(arg_ty, IFACE_NAME, iface_def);
                Some(())
            }

            FormatSpec::Debug => {
                // FIXME: Fix this (WellKnownInterfacesMap is deprecated)

                // self.check_implements_interface(arg_ty, IFACE_NAME, iface_def);
                Some(())
            }

            FormatSpec::Hex | FormatSpec::Oct | FormatSpec::Bin => {
                match self.result.interner.get(arg_ty) {
                    Type::Builtin(b) if coerce::builtin_is_integer(*b) => {}
                    Type::IntLiteral => {}
                    Type::Error => {}
                    _ => {
                        self.report(TypeError::FormatRequiresInteger {
                            found: self
                                .result
                                .interner
                                .display_type(arg_ty, Rc::clone(&self.interner), self.resolution)
                                .into(),
                            src: source.src(),
                            span: source.span,
                        });
                        return None;
                    }
                }

                Some(())
            }

            FormatSpec::Float { .. } => {
                match self.result.interner.get(arg_ty) {
                    Type::Builtin(b) if coerce::builtin_is_float(*b) => {}
                    Type::FloatLiteral => {}
                    Type::Error => {}
                    _ => {
                        self.report(TypeError::FormatRequiresFloat {
                            found: self
                                .result
                                .interner
                                .display_type(arg_ty, Rc::clone(&self.interner), self.resolution)
                                .into(),
                            src: source.src(),
                            span: source.span,
                        });
                        return None;
                    }
                }

                Some(())
            }
        }
    }

    fn well_known_or_report(
        &mut self,
        name: &str,
        val: Option<DefId>,
        source: Source,
    ) -> Option<DefId> {
        if val.is_none() {
            self.report(TypeError::InterfaceNotAvaible {
                name: name.into(),
                src: source.src(),
                span: source.span,
            });
        }

        val
    }

    // << Macros

    fn check_field_access(
        &mut self,
        id: HirId,
        object: &HirExpr,
        field: (Spur, SourceSpan),
    ) -> TypeId {
        let obj_ty = self.synth_expr(object);
        let (field_name, field_span) = field;

        let struct_def = match self.result.interner.get(obj_ty) {
            Type::Struct { def_id, .. } => def_id,
            Type::Pointer { inner, .. } => match self.result.interner.get(*inner) {
                Type::Struct { def_id, .. } => def_id,
                Type::Error => return self.result.interner.error(),
                _ => {
                    self.result.errors.push(TypeError::NotAStruct {
                        provided: self.display_type(obj_ty).into(),
                        src: object.source.src(),
                        span: field_span,
                    });
                    return self.result.interner.error();
                }
            },

            Type::Error => return self.result.interner.error(),

            _ => {
                self.result.errors.push(TypeError::NotAStruct {
                    provided: self.display_type(obj_ty).into(),
                    src: object.source.src(),
                    span: field_span,
                });
                return self.result.interner.error();
            }
        };

        let Some(info) = self.result.struct_info.get(struct_def) else {
            return self.result.interner.error();
        };

        match info
            .fields
            .iter()
            .find(|(n, _, _)| *n == field_name)
            .map(|(_, d, t)| (*d, *t))
        {
            Some((field_def, ty)) => {
                self.result.field_resolutions.insert(id, field_def);
                ty
            }
            None => {
                let interner = self.interner.borrow();

                let struct_name_id = self.resolution.defs.get(struct_def).expect("wtf").name;

                let struct_name = interner.resolve(&struct_name_id).into();
                let field_name = interner.resolve(&field_name).into();

                drop(interner);

                self.result.errors.push(TypeError::UnknownField {
                    struct_name,
                    field: field_name,
                    src: object.source.src(),
                    span: field_span,
                });
                self.result.interner.error()
            }
        }
    }

    fn check_implements_interface(
        &mut self,
        ty: TypeId,
        interface_name: &str,
        interface_def: DefId,
    ) -> bool {
        match self.result.interner.get(ty) {
            Type::Builtin(b) => match b {
                BuiltinType::i8
                | BuiltinType::i16
                | BuiltinType::i32
                | BuiltinType::i64
                | BuiltinType::isize
                | BuiltinType::u8
                | BuiltinType::u16
                | BuiltinType::u32
                | BuiltinType::u64
                | BuiltinType::usize
                | BuiltinType::f32
                | BuiltinType::f64 => [
                    "Display", "Debug", "Copy", "Add", "Sub", "Mul", "Div", "Neg", "Not",
                ]
                .contains(&interface_name),

                BuiltinType::bool | BuiltinType::char => {
                    ["Display", "Debug", "Copy"].contains(&interface_name)
                }

                BuiltinType::void => false,
            },

            Type::Pointer { .. } => [
                "Debug",
                "Copy",
                "Add",
                "Sub",
                "Deref",
                "DerefAssign",
                "Slice",
                "SliceAssign",
            ]
            .contains(&interface_name),

            Type::Struct { def_id, .. } | Type::Enum { def_id } => self
                .resolution
                .impls
                .contains_key(&(*def_id, interface_def)),

            // soon... (or i'll find another found to check bounds)
            Type::GenericParam(_) => false,

            Type::Array { element, .. } | Type::Slice { element } => {
                let copy_or_drop = if matches!(interface_name, "Copy" | "Drop") {
                    self.check_implements_interface(*element, interface_name, interface_def)
                } else {
                    false
                };

                ["Slice", "SliceAssign"].contains(&interface_name) || copy_or_drop
            }

            Type::IntLiteral | Type::FloatLiteral => [
                "Display", "Debug", "Copy", "Add", "Sub", "Mul", "Div", "Neg", "Not",
            ]
            .contains(&interface_name),

            Type::Error | Type::Never => true,

            Type::Void | Type::Interface { .. } | Type::Fn { .. } => false,

            _ => todo!(),
        }
    }

    fn check_expr(&mut self, expr: &HirExpr, expected: TypeId, allow_const_remove: bool) -> TypeId {
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

        self.coerce_or_error(
            actual,
            expected,
            expr.source.clone(),
            expr.id,
            allow_const_remove,
        )
    }

    fn coerce_or_error(
        &mut self,
        actual: TypeId,
        expected: TypeId,
        source: Source,
        id: HirId,

        allow_const_remove: bool,
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

            CoerceResult::RemoveConst => {
                if !allow_const_remove {
                    self.report(TypeError::Mismatch {
                        expected: self.display_type(expected).into(),
                        found: self.display_type(actual).into(),
                        src: source.src(),
                        span: source.span,
                    });
                }

                expected
            }

            CoerceResult::Fail => {
                self.report(TypeError::Mismatch {
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

                let object_is_const_ptr = self
                    .result
                    .expr_types
                    .get(&object.id)
                    .map(|&ty| {
                        matches!(
                            self.result.interner.get(ty),
                            Type::Pointer { is_const: true, .. }
                        )
                    })
                    .unwrap_or(false);

                field_const || object_is_const_ptr || self.find_const_violation(object)
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

    fn check_call(
        &mut self,
        call_id: HirId,
        callee: &HirExpr,
        args: &[Rc<HirExpr>],
        explicit_generic_args: &[Rc<HirTypeExpr>],
        source: Source,
    ) -> TypeId {
        if let HirExprKind::FieldAccess { object, field } = &callee.kind
            && let Some(result) = self.try_check_method_call(
                call_id,
                object,
                *field,
                args,
                explicit_generic_args,
                source.clone(),
            )
        {
            return result;
        }

        let callee_def = match &callee.kind {
            HirExprKind::VarRef(def_id) => Some(*def_id),
            _ => None,
        };

        let Some(def_id) = callee_def.filter(|d| self.fn_sigs.contains_key(d)) else {
            let callee_ty = self.synth_expr(callee);
            return self.check_call_via_fn_type(callee_ty, args, source);
        };

        let sig = &self.fn_sigs[&def_id];

        let sig_params = sig.params.clone();
        let sig_ret = sig.ret;
        let sig_generics = sig.generics.clone();

        self.result
            .record_expr_type(callee.id, self.result.def_types[&def_id]);

        if args.len() != sig_params.len() {
            self.report(TypeError::ArgCountMismatch {
                expected: sig_params.len(),
                found: args.len(),
                src: source.src(),
                span: source.span,
            });
        }

        let mut bindings: HashMap<DefId, TypeId> = HashMap::new();

        for (g, explicit) in sig_generics.iter().zip(explicit_generic_args.iter()) {
            let ty = self.lower_hir_type(explicit);
            bindings.insert(*g, ty);
        }

        for (param_ty, arg) in sig_params.iter().zip(args.iter()) {
            self.infer_or_check_arg(*param_ty, arg, &mut bindings, source.clone());
        }

        for g in &sig_generics {
            if !bindings.contains_key(g) {
                let declared = &self.resolution.defs.get(g).expect("wtf").span;

                self.report(TypeError::CannotInferGeneric {
                    src: source.src(),
                    span: source.span,
                    declared: vec![error::InferGenericDeclared {
                        src: declared.src(),
                        span: declared.span,
                    }],
                });

                bindings.insert(*g, self.result.interner.error());
            }
        }

        for g in &sig_generics {
            let concrete_ty = bindings[g];
            let bounds = self.fn_sigs[&def_id]
                .generic_bounds
                .get(g)
                .cloned()
                .unwrap_or_default();

            for iface_def in bounds {
                if !self.type_satisfies_interface(concrete_ty, iface_def) {
                    let interner = self.interner.borrow();

                    let generic = interner.resolve(&self.resolution.defs[g].name).into();
                    let bound = interner
                        .resolve(&self.resolution.defs[&iface_def].name)
                        .into();
                    let ty = self.display_type(concrete_ty).into();

                    drop(interner);

                    self.report(TypeError::GenericBoundNotSatisfied {
                        generic,
                        bound,
                        ty,
                        src: source.src(),
                        span: source.span,
                    });
                }
            }
        }

        let resolved_generic_args: Vec<TypeId> = sig_generics.iter().map(|g| bindings[g]).collect();

        self.result.call_resolutions.insert(
            call_id,
            CallResolution {
                fn_def: def_id,
                generic_args: resolved_generic_args,
            },
        );

        self.substitute_generics(sig_ret, &bindings)
    }

    fn try_check_method_call(
        &mut self,
        call_id: HirId,
        object: &HirExpr,
        field: (Spur, SourceSpan),
        args: &[Rc<HirExpr>],
        explicit_generic_args: &[Rc<HirTypeExpr>],
        source: Source,
    ) -> Option<TypeId> {
        let (field_name, field_span) = field;

        let obj_ty = self.synth_expr(object);

        let (struct_def, obj_is_ptr, ptr_is_const) = match self.result.interner.get(obj_ty).clone()
        {
            Type::Struct { def_id, .. } => (def_id, false, false),
            Type::Pointer { inner, is_const } => match self.result.interner.get(inner) {
                Type::Struct { def_id, .. } => (*def_id, true, is_const),
                _ => return None,
            },
            _ => return None,
        };

        let is_field = self
            .result
            .struct_info
            .get(&struct_def)
            .map(|info| info.fields.iter().any(|(n, _, _)| *n == field_name))
            .unwrap_or(false);

        if is_field {
            return None;
        }

        let method_def_id = *self.struct_methods.get(&struct_def)?.get(&field_name)?;
        let sig = &self.fn_sigs[&method_def_id];

        let sig_params = sig.params.clone();
        let sig_ret = sig.ret;
        let sig_generics = sig.generics.clone();
        let self_mode = sig.self_mode;

        if args.len() != sig_params.len() {
            self.report(TypeError::ArgCountMismatch {
                expected: sig_params.len(),
                found: args.len(),
                src: source.src(),
                span: source.span,
            });
        }

        match self_mode {
            Some(SelfMode::Value) | Some(SelfMode::ValueConst) if obj_is_ptr => {
                self.report(TypeError::CannotMoveThroughPointer {
                    src: source.src(),
                    span: field_span,
                });
            }

            Some(SelfMode::RefMut) if obj_is_ptr && ptr_is_const => {
                self.report(TypeError::AssignToConst {
                    src: source.src(),
                    span: field_span,
                });
            }

            _ => {}
        }

        let mut bindings: HashMap<DefId, TypeId> = HashMap::new();

        for (g, explicit) in sig_generics.iter().zip(explicit_generic_args.iter()) {
            let ty = self.lower_hir_type(explicit);
            bindings.insert(*g, ty);
        }

        for (param_ty, arg) in sig_params.iter().zip(args.iter()) {
            self.infer_or_check_arg(*param_ty, arg, &mut bindings, source.clone());
        }

        for g in &sig_generics {
            if !bindings.contains_key(g) {
                let declared = &self.resolution.defs.get(g).expect("wtf").span;

                self.report(TypeError::CannotInferGeneric {
                    src: source.src(),
                    span: source.span,
                    declared: vec![error::InferGenericDeclared {
                        src: declared.src(),
                        span: declared.span,
                    }],
                });

                bindings.insert(*g, self.result.interner.error());
            }
        }

        for g in &sig_generics {
            let concrete_ty = bindings[g];
            let bounds = self.fn_sigs[&method_def_id]
                .generic_bounds
                .get(g)
                .cloned()
                .unwrap_or_default();

            for iface_def in bounds {
                if !self.type_satisfies_interface(concrete_ty, iface_def) {
                    let interner = self.interner.borrow();

                    let generic = interner.resolve(&self.resolution.defs[g].name).into();
                    let bound = interner
                        .resolve(&self.resolution.defs[&iface_def].name)
                        .into();
                    let ty = self.display_type(concrete_ty).into();

                    drop(interner);

                    self.report(TypeError::GenericBoundNotSatisfied {
                        generic,
                        bound,
                        ty,
                        src: source.src(),
                        span: source.span,
                    });
                }
            }
        }

        let resolved_generic_args: Vec<TypeId> = sig_generics.iter().map(|g| bindings[g]).collect();

        self.result.call_resolutions.insert(
            call_id,
            CallResolution {
                fn_def: method_def_id,
                generic_args: resolved_generic_args,
            },
        );

        Some(self.substitute_generics(sig_ret, &bindings))
    }

    fn check_call_via_fn_type(
        &mut self,
        callee_ty: TypeId,
        args: &[Rc<HirExpr>],
        source: Source,
    ) -> TypeId {
        match self.result.interner.get(callee_ty).clone() {
            Type::Fn { params, ret } => {
                if args.len() != params.len() {
                    self.report(TypeError::ArgCountMismatch {
                        expected: params.len(),
                        found: args.len(),
                        src: source.src(),
                        span: source.span,
                    });
                }

                for (param_ty, arg) in params.iter().zip(args.iter()) {
                    self.check_expr(arg, *param_ty, false);
                }

                ret
            }

            Type::Error => self.result.interner.error(),

            _ => {
                self.report(TypeError::NotCallable {
                    ty: self.display_type(callee_ty).into(),
                    src: source.src(),
                    span: source.span,
                });
                self.result.interner.error()
            }
        }
    }

    fn infer_or_check_arg(
        &mut self,
        param_ty: TypeId,
        arg: &HirExpr,
        bindings: &mut HashMap<DefId, TypeId>,
        source: Source,
    ) {
        if self.type_contains_generic(param_ty) {
            let arg_ty = self.synth_expr(arg);
            let arg_ty = self.default_literal(arg_ty);
            self.result.record_expr_type(arg.id, arg_ty);
            self.unify_for_inference(param_ty, arg_ty, bindings, source);
        } else {
            let substituted = self.substitute_generics(param_ty, bindings);
            self.check_expr(arg, substituted, false);
        }
    }

    fn type_contains_generic(&self, ty: TypeId) -> bool {
        match self.result.interner.get(ty) {
            Type::GenericParam(_) => true,
            Type::Pointer { inner, .. } => self.type_contains_generic(*inner),
            Type::Array { element, .. } | Type::Slice { element } => {
                self.type_contains_generic(*element)
            }
            Type::Struct { generic_args, .. } => {
                generic_args.iter().any(|a| self.type_contains_generic(*a))
            }
            Type::Fn { params, ret } => {
                params.iter().any(|p| self.type_contains_generic(*p))
                    || self.type_contains_generic(*ret)
            }
            _ => false,
        }
    }

    // I just hope I'll never touch this code again
    fn unify_for_inference(
        &mut self,
        param_ty: TypeId,
        arg_ty: TypeId,
        bindings: &mut HashMap<DefId, TypeId>,
        source: Source,
    ) {
        match (self.result.interner.get(param_ty).clone(), self.result.interner.get(arg_ty).clone()) {
            (Type::GenericParam(g), _) => match bindings.get(&g) {
                Some(&existing) if existing != arg_ty && !try_coerce(&self.result.interner, arg_ty, existing).is_ok() => {
                    self.result.errors.push(TypeError::GenericConflict {
                        param: self.display_type(param_ty).into(),
                        first: self.display_type(existing).into(),
                        second: self.display_type(arg_ty).into(),
                        src: source.src(),
                        span: source.span,
                    });
                }

                Some(_) => {},
                None => {
                    bindings.insert(g, arg_ty);
                }
            }

            (Type::Pointer { inner: pinner, .. }, Type::Pointer { inner: ainner, .. }) => {
                self.unify_for_inference(pinner, ainner, bindings, source);
            }

            (Type::Array { element: pelem, .. }, Type::Array { element: aelem, .. })
            | (Type::Slice { element: pelem }, Type::Slice { element: aelem })
            | (Type::Array { element: pelem, .. }, Type::Slice { element: aelem })
            | (Type::Slice { element: pelem }, Type::Array { element: aelem, .. }) => {
                self.unify_for_inference(pelem, aelem, bindings, source);
            }

            (Type::Struct { def_id: pd, generic_args: pa }, Type::Struct { def_id: ad, generic_args: aa }) if pd == ad => {
                for (p, a) in pa.iter().zip(aa.iter()) {
                    self.unify_for_inference(*p, *a, bindings, source.clone());
                }
            }

            (Type::Fn { params: pp, ret: pr }, Type::Fn { params: ap, ret: ar }) if pp.len() == ap.len() => {
                for (p, a) in pp.iter().zip(ap.iter()) {
                    self.unify_for_inference(*p, *a, bindings, source.clone());
                }
                self.unify_for_inference(pr, ar, bindings, source);
            }

            _ => {}
        }
    }

    fn type_satisfies_interface(&self, ty: TypeId, iface_def: DefId) -> bool {
        match self.result.interner.get(ty).clone() {
            Type::Error => true,

            Type::Struct { def_id, .. } => self.resolution.impls.contains_key(&(def_id, iface_def)),
            Type::Enum { def_id } => self.resolution.impls.contains_key(&(def_id, iface_def)),

            _ => false,
        }
    }

    fn substitute_generics(
        &mut self,
        ty: TypeId,
        bindings: &HashMap<DefId, TypeId>,
    ) -> TypeId {
        match self.result.interner.get(ty).clone() {
            Type::GenericParam(g) => bindings.get(&g).copied().unwrap_or(ty),

            Type::Pointer { inner, is_const } => {
                let new_inner = self.substitute_generics(inner, bindings);
                if new_inner == inner {
                    ty
                } else {
                    self.result.interner.intern(Type::Pointer { inner: new_inner, is_const })
                }
            }

            Type::Array { element, len } => {
                let new_elem = self.substitute_generics(element, bindings);
                if new_elem == element {
                    ty
                } else {
                    self.result.interner.intern(Type::Array { element: new_elem, len })
                }
            }

            Type::Slice { element } => {
                let new_elem = self.substitute_generics(element, bindings);
                if new_elem == element {
                    ty
                } else {
                    self.result.interner.intern(Type::Slice { element: new_elem })
                }
            }

            Type::Struct { def_id, generic_args } => {
                let new_args: Vec<TypeId> = generic_args
                    .iter()
                    .map(|a| self.substitute_generics(*a, bindings))
                    .collect();
                if new_args == generic_args {
                    ty
                } else {
                    self.result.interner.intern(Type::Struct { def_id, generic_args: new_args })
                }
            }

            Type::Fn { params, ret } => {
                let new_params: Vec<TypeId> = params
                    .iter()
                    .map(|p| self.substitute_generics(*p, bindings))
                    .collect();
                let new_ret = self.substitute_generics(ret, bindings);
                if new_params == params && new_ret == ret {
                    ty
                } else {
                    self.result.interner.intern(Type::Fn { params: new_params, ret: new_ret })
                }
            }

            _ => ty,
        }
    }

    fn check_binary_op(
        &mut self,
        op: BinaryOp,
        lhs: TypeId,
        rhs: TypeId,
        source: Source,
    ) -> TypeId {
        if lhs == self.result.interner.error() || rhs == self.result.interner.error() {
            return self.result.interner.error();
        }

        let unified = if lhs == rhs {
            Some(lhs)
        } else if let Type::Pointer { .. } = self.result.interner.get(lhs)
            && let Type::Builtin(BuiltinType::usize) = self.result.interner.get(rhs)
        {
            Some(lhs)
        } else if try_coerce(&self.result.interner, lhs, rhs).is_ok() {
            Some(rhs)
        } else if try_coerce(&self.result.interner, rhs, lhs).is_ok() {
            Some(lhs)
        } else {
            None
        };

        let Some(operand_ty) = unified else {
            self.report(TypeError::BinaryNotSupported {
                op,
                lhs_type: self.display_type(lhs).into(),
                rhs_type: self.display_type(rhs).into(),
                src: source.src(),
                span: source.span,
            });
            return self.result.interner.error();
        };

        use BinaryOp::*;

        match op {
            Add | Sub | Mul | Div | Mod | BitAnd | BitOr | BitXor | Shl | Shr => {
                if self.is_numeric_or_literal(operand_ty) {
                    return operand_ty;
                }

                match self.result.interner.get(operand_ty) {
                    Type::Struct { def_id, .. } => {
                        // FIXME: Fix this (WellKnownInterfacesMap is deprecated)

                        let iface_name = format!("{:?}", op);

                        // if let Some(iface_def) = self.well_known_or_report(
                        //     &iface_name,
                        //     self.well_known.get(&iface_name),
                        //     source.clone(),
                        // ) {
                        //     if self.check_implements_interface(operand_ty, &iface_name, iface_def) {
                        //         return operand_ty;
                        //     }
                        //
                        //     self.report(TypeError::InterfaceNotImplemented {
                        //         name: iface_name.into(),
                        //         ty_name: self.display_type(operand_ty).into(),
                        //         src: source.src(),
                        //         span: source.span,
                        //     });
                        //
                        //     return self.result.interner.error();
                        // }

                        self.result.interner.error()
                    }

                    Type::Pointer { .. } if matches!(op, Add | Sub) => operand_ty,

                    _ => {
                        self.report(TypeError::BinaryNotSupported {
                            op,
                            lhs_type: self.display_type(lhs).into(),
                            rhs_type: self.display_type(rhs).into(),
                            src: source.src(),
                            span: source.span,
                        });
                        self.result.interner.error()
                    }
                }
            }

            Eq | Ne | Lt | Gt | Le | Ge => {
                match self.result.interner.get(operand_ty) {
                    Type::Builtin(_) => {}
                    Type::Pointer { .. } => {}
                    Type::Enum { .. } => {}
                    Type::IntLiteral | Type::FloatLiteral => {}
                    Type::Struct { .. } => {
                        // FIXME: Fix this (WellKnownInterfacesMap is deprecated)

                        // if let Some(iface_def) = self.well_known_or_report(
                        //     IFACE_NAME,
                        //     self.well_known.get(IFACE_NAME),
                        //     source.clone(),
                        // ) {
                        //     if self.check_implements_interface(operand_ty, IFACE_NAME, iface_def) {
                        //         return operand_ty;
                        //     }
                        //
                        //     self.report(TypeError::InterfaceNotImplemented {
                        //         name: IFACE_NAME.into(),
                        //         ty_name: self.display_type(operand_ty).into(),
                        //         src: source.src(),
                        //         span: source.span,
                        //     });
                        // }
                    }

                    _ => {
                        self.report(TypeError::BinaryNotSupported {
                            op,
                            lhs_type: self.display_type(lhs).into(),
                            rhs_type: self.display_type(rhs).into(),
                            src: source.src(),
                            span: source.span,
                        });
                    }
                }

                self.result.interner.builtin(BuiltinType::bool)
            }

            LogicalAnd | LogicalOr => {
                let bool_ty = self.result.interner.builtin(BuiltinType::bool);

                if operand_ty != bool_ty {
                    self.report(TypeError::BinaryNotSupported {
                        op,
                        lhs_type: self.display_type(lhs).into(),
                        rhs_type: self.display_type(rhs).into(),
                        src: source.src(),
                        span: source.span,
                    });
                }

                bool_ty
            }
        }
    }

    fn check_unary_op(&mut self, op: UnaryOp, operand: TypeId, source: Source) -> TypeId {
        if matches!(self.result.interner.get(operand), Type::Error) {
            return self.result.interner.error();
        }

        match op {
            UnaryOp::Neg => {
                if self.is_numeric_or_literal(operand) {
                    return operand;
                }

                match self.result.interner.get(operand) {
                    Type::Struct { def_id, .. } => {
                        // FIXME: Fix this (WellKnownInterfacesMap is deprecated)

                        // if let Some(iface_def) = self.well_known_or_report(
                        //     IFACE_NAME,
                        //     self.well_known.get(IFACE_NAME),
                        //     source.clone(),
                        // ) {
                        //     if self.check_implements_interface(operand, IFACE_NAME, iface_def) {
                        //         return operand;
                        //     }
                        //
                        //     self.report(TypeError::InterfaceNotImplemented {
                        //         name: IFACE_NAME.into(),
                        //         ty_name: self.display_type(operand).into(),
                        //         src: source.src(),
                        //         span: source.span,
                        //     });
                        // }

                        self.result.interner.error()
                    }

                    _ => {
                        self.report(TypeError::UnaryNotSupported {
                            op,
                            child_type: self.display_type(operand).into(),
                            src: source.src(),
                            span: source.span,
                        });
                        self.result.interner.error()
                    }
                }
            }

            UnaryOp::Not => {
                let bool_ty = self.result.interner.builtin(BuiltinType::bool);

                if operand == bool_ty {
                    return bool_ty;
                };

                match self.result.interner.get(operand) {
                    Type::Struct { def_id, .. } => {
                        // FIXME: Fix this (WellKnownInterfacesMap is deprecated)

                        const IFACE_NAME: &str = "Not";

                        // if let Some(iface_def) = self.well_known_or_report(
                        //     IFACE_NAME,
                        //     self.well_known.get(IFACE_NAME),
                        //     source.clone(),
                        // ) {
                        //     if self.check_implements_interface(operand, IFACE_NAME, iface_def) {
                        //         return operand;
                        //     }
                        //
                        //     self.report(TypeError::InterfaceNotImplemented {
                        //         name: IFACE_NAME.into(),
                        //         ty_name: self.display_type(operand).into(),
                        //         src: source.src(),
                        //         span: source.span,
                        //     });
                        // }

                        self.result.interner.error()
                    }

                    _ => {
                        self.report(TypeError::UnaryNotSupported {
                            op,
                            child_type: self.display_type(operand).into(),
                            src: source.src(),
                            span: source.span,
                        });
                        self.result.interner.error()
                    }
                }
            }

            UnaryOp::BitNot => {
                if self.is_numeric_or_literal(operand) {
                    operand
                } else {
                    self.report(TypeError::UnaryNotSupported {
                        op,
                        child_type: self.display_type(operand).into(),
                        src: source.src(),
                        span: source.span,
                    });
                    self.result.interner.error()
                }
            }

            UnaryOp::Deref => match self.result.interner.get(operand) {
                Type::Pointer { inner, .. } => *inner,

                Type::Struct { def_id, .. } => {
                    // TODO: Add type inference from interface function (generic params must be included)
                    // FIXME: Fix this (WellKnownInterfacesMap is deprecated)

                    let iface_name = if self.expect_assign_interface {
                        "DerefAssign"
                    } else {
                        "Deref"
                    };

                    // if let Some(iface_def) = self.well_known_or_report(
                    //     iface_name,
                    //     self.well_known.get(iface_name),
                    //     source.clone(),
                    // ) {
                    //     if self.check_implements_interface(operand, iface_name, iface_def) {
                    //         return operand;
                    //     }
                    //
                    //     self.report(TypeError::InterfaceNotImplemented {
                    //         name: iface_name.into(),
                    //         ty_name: self.display_type(operand).into(),
                    //         src: source.src(),
                    //         span: source.span,
                    //     });
                    // }

                    self.result.interner.error()
                }

                _ => {
                    self.report(TypeError::UnaryNotSupported {
                        op,
                        child_type: self.display_type(operand).into(),
                        src: source.src(),
                        span: source.span,
                    });
                    self.result.interner.error()
                }
            },

            UnaryOp::AddrOf => self.result.interner.intern(Type::Pointer {
                inner: operand,
                is_const: false,
            }),
        }
    }

    fn is_numeric_or_literal(&self, ty: TypeId) -> bool {
        match self.result.interner.get(ty) {
            Type::IntLiteral | Type::FloatLiteral => true,
            Type::Builtin(b) => coerce::builtin_is_integer(*b) || coerce::builtin_is_float(*b),
            _ => false,
        }
    }
}

fn format_error_to_diagnostic(err: &FormatParseError, format_source: &Source) -> TypeError {
    match err {
        FormatParseError::UnclosedBrace { offset } => {
            let src = format_source.src();
            let span: miette::SourceSpan = (format_source.span.offset() + 1 + offset, 1).into();

            TypeError::FormatParseError {
                message: "unclosed '{' in format string".into(),
                src,
                span,
            }
        }

        FormatParseError::UnknownSpecifier { spec, offset } => {
            let src = format_source.src();
            let span: miette::SourceSpan =
                (format_source.span.offset() + 1 + offset, spec.len()).into();

            TypeError::FormatParseError {
                message: format!("unknown format specifier: '{}'", spec).into(),
                src,
                span,
            }
        }

        FormatParseError::InvalidPrecision { raw, offset } => {
            let src = format_source.src();
            let span: miette::SourceSpan =
                (format_source.span.offset() + 1 + offset, raw.len()).into();

            TypeError::FormatParseError {
                message: format!("invalid precision '{raw}' in: `{{:.{raw}}}`").into(),
                src,
                span,
            }
        }
    }
}
