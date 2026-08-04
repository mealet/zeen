use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use lasso::Spur;
use miette::SourceSpan;
use zeen_ast::Source;

use crate::{
    coerce::{CoerceResult, try_coerce},
    context::{FnCtx, InterfaceRegistry, TypeCheckCtx},
    format_str::FormatSpec,
    result::{CallResolution, OperatorResolution, TypeCheckResult},
};
use crate::{error::TypeError, format_str::FormatParseError};

use zeen_ast::{
    expressions::{BinaryOp, Literal, UnaryOp},
    types::BuiltinType,
};
use zeen_driver::{CompilationContext, CompilationOutput};
use zeen_hir::{
    HirId, HirModule,
    decl::{HirDecl, HirDeclKind, HirFn},
    expr::{HirExpr, HirExprKind, HirFieldInit, HirMacroKind},
    stmt::{HirStmt, HirStmtKind},
    types::{HirTypeExpr, HirTypeKind},
};
use zeen_resolve::{DefId, DefKind, ResolutionResult};
use zeen_types::{
    Capabilities, ReceiverAccess, SelfMode, StructFieldInfo, StructTypeInfo, Type, TypeId,
    binary_op_interface, self_mode_of, unary_op_interface,
};

pub mod coerce;
pub mod context;
pub mod error;
pub mod format_str;
pub mod result;

pub const DEFAULT_INT_LITERAL: BuiltinType = BuiltinType::i32;
pub const DEFAULT_FLOAT_LITERAL: BuiltinType = BuiltinType::f64;

pub struct TypeChecker<'res> {
    resolution: &'res mut ResolutionResult,
    compilation_context: &'res CompilationContext,
    expect_assign_interface: bool,
    found_main_fn: bool,

    result: TypeCheckResult,
    ctx: TypeCheckCtx,
    interner: Rc<RefCell<lasso::Rodeo>>,
    errors: Vec<TypeError>,

    fn_sigs: HashMap<DefId, FnSignature>,

    interface_registry: InterfaceRegistry,
    interface_methods: HashMap<DefId, Vec<DefId>>,
    interface_generics: HashMap<DefId, Vec<DefId>>,

    struct_generics: HashMap<DefId, Vec<DefId>>,
    struct_methods: HashMap<DefId, HashMap<Spur, DefId>>,
    enum_variants: HashMap<DefId, Vec<DefId>>,

    impl_generic_to_struct_generic: HashMap<(DefId, DefId), HashMap<DefId, DefId>>,
    method_owning_interface: HashMap<DefId, DefId>,
}

struct FnSignature {
    params: Vec<TypeId>,
    ret: TypeId,
    generics: Vec<DefId>,
    generic_bounds: HashMap<DefId, Vec<DefId>>,
    self_mode: Option<SelfMode>,
    is_pub: bool,
    is_variadic: bool,
}

struct InterfaceCallResult {
    pub ret_ty: TypeId,
    pub method_def: DefId,
}

impl<'res> TypeChecker<'res> {
    pub fn new(
        resolution: &'res mut ResolutionResult,
        compilation_context: &'res CompilationContext,
        interner: Rc<RefCell<lasso::Rodeo>>,
    ) -> Self {
        let interface_registry = InterfaceRegistry::build(resolution, &interner);

        Self {
            resolution,
            compilation_context,
            result: TypeCheckResult::default(),
            ctx: TypeCheckCtx::new(),
            errors: Vec::new(),
            expect_assign_interface: false,
            found_main_fn: false,
            interner,
            fn_sigs: HashMap::new(),
            interface_registry,
            interface_methods: HashMap::new(),
            interface_generics: HashMap::new(),
            struct_generics: HashMap::new(),
            struct_methods: HashMap::new(),
            enum_variants: HashMap::new(),
            impl_generic_to_struct_generic: HashMap::new(),
            method_owning_interface: HashMap::new(),
        }
    }

    pub fn finish(self) -> Result<TypeCheckResult, Vec<TypeError>> {
        if self.errors.is_empty() {
            return Ok(self.result);
        }
        Err(self.errors)
    }

    // --> Helpers

    fn def_kind(&self, def_id: DefId) -> Option<&DefKind> {
        self.resolution.defs.get(&def_id).map(|info| &info.kind)
    }

    fn report(&mut self, err: TypeError) {
        self.errors.push(err);
    }

    fn display_type(&self, id: TypeId) -> String {
        self.result
            .interner
            .display_type(id, Rc::clone(&self.interner), self.resolution)
    }

    fn format_signature(&self, method_name: &str, params: &[TypeId], ret: TypeId) -> String {
        let param_strs: Vec<String> = params.iter().map(|&p| self.display_type(p)).collect();
        let params_joined = param_strs.join(", ");

        format!(
            "fn {}({}) {}",
            method_name,
            params_joined,
            self.display_type(ret)
        )
    }

    // --> Entry Point

    pub fn check_module(&mut self, module: &HirModule) {
        // Few words here: we're doing multiple passes here:
        // 0. Register struct generic params (for lower_hir_type_inner)
        // 1. Declare signatures
        // 2. Check and infer if structs have Copy and Drop capabilities
        // 3. Check declarations bodies
        // 4. Verify that we have main function if required

        for decl in &module.decls {
            if let HirDeclKind::Struct(s) = &decl.kind {
                self.struct_generics
                    .insert(decl.def_id, s.generics.iter().map(|g| g.def_id).collect());
            }
        }

        for decl in &module.decls {
            self.declare_signature(decl);
        }

        for decl in &module.decls {
            self.compute_structs_capabilities(decl);
        }

        for decl in &module.decls {
            self.check_decl_body(decl);
        }

        if self.compilation_context.output == CompilationOutput::Binary && !self.found_main_fn {
            self.report(TypeError::MainNotFound {
                src: module.decls[0].source.src(),
            });
        }
    }

    // > Pass 1

    fn declare_signature(&mut self, decl: &HirDecl) {
        match &decl.kind {
            HirDeclKind::Fn(hir_fn) => {
                if hir_fn.name.0 == self.interner.borrow_mut().get_or_intern("main") {
                    self.found_main_fn = true;
                    self.result.main_fn_def = Some(decl.def_id);

                    let signature_matches =
                        hir_fn.params.is_empty() && !hir_fn.is_extern && hir_fn.generics.is_empty();

                    if !signature_matches {
                        self.report(TypeError::MainSignatureMismatch {
                            src: decl.source.src(),
                            span: hir_fn.name.1,
                        });
                    }
                }

                self.declare_fn_signature(decl.def_id, hir_fn);
            }

            HirDeclKind::Struct(s) => {
                // Code below is moved to `check_module` parent function (Pass 0).
                //
                // self.struct_generics
                //     .insert(decl.def_id, s.generics.iter().map(|g| g.def_id).collect());

                let mut fields = Vec::with_capacity(s.fields.len());

                for field in &s.fields {
                    let (ty, is_const) = self.lower_hir_type_with_const(&field.ty);

                    self.result.def_types.insert(field.def_id, ty);
                    self.result.const_bindings.insert(field.def_id, is_const);

                    // inifinite recursive type checker

                    if let Type::Struct { def_id, .. } = self.result.interner.get(ty)
                        && def_id == &decl.def_id
                    {
                        self.report(TypeError::InfiniteRecursiveType {
                            ty: self.display_type(ty).into(),
                            src: decl.source.src(),
                            span: field.ty.source.span,
                        });
                    }

                    fields.push(StructFieldInfo {
                        name: field.name,
                        field_def: field.def_id,
                        field_ty: ty,
                        struct_def: decl.def_id,
                        is_pub: field.is_pub,
                    });
                }

                // for something like: `let a: Foo = Foo;`
                self.result
                    .def_types
                    .insert(decl.def_id, self.result.interner.void());

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
                self.interface_generics
                    .insert(decl.def_id, i.generics.iter().map(|g| g.def_id).collect());

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

                let variant_ids: Vec<DefId> = e.variants.iter().map(|v| v.def_id).collect();
                self.enum_variants.insert(decl.def_id, variant_ids);

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
        let params_len = hir_fn.params.len();

        let mut is_variadic = false;

        let params: Vec<TypeId> = hir_fn
            .params
            .iter()
            .enumerate()
            .map(|(idx, param)| {
                if matches!(param.ty.kind, HirTypeKind::VaArgs) && !is_variadic {
                    is_variadic = true;

                    if idx != params_len - 1 {
                        self.report(TypeError::InvalidVaArgs {
                            src: param.ty.source.src(),
                            span: param.span,
                        });
                    }

                    if !hir_fn.is_extern {
                        self.report(TypeError::NonExternVaArgs {
                            src: param.ty.source.src(),
                            span: param.span,
                        });
                    }
                }

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
                is_pub: hir_fn.is_pub,
                is_variadic,
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

                if let Some(iface_def) = imp.interface {
                    self.method_owning_interface
                        .insert(method.def_id, iface_def);
                }
            }
        }

        let Some(object_def) = imp.object else { return };

        // implement block on enum
        if !matches!(self.def_kind(object_def), Some(DefKind::Struct)) {
            self.report(TypeError::ImplementNonStruct {
                src: source.src(),
                span: imp.object_bindings_span,
            });
            return;
        }

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
        } else {
            if let Some(iface_def) = imp.interface {
                let mapping: HashMap<DefId, DefId> = imp
                    .object_generics_bindings
                    .iter()
                    .copied()
                    .zip(struct_generics.iter().copied())
                    .collect();

                self.impl_generic_to_struct_generic
                    .insert((object_def, iface_def), mapping);
            }
        }

        let Some(iface_def) = imp.interface else {
            return;
        };

        let imp_generics: Vec<DefId> = imp.generics.iter().map(|g| g.def_id).collect();

        self.check_implement_matches_interface(
            imp,
            iface_def,
            object_def,
            &imp_generics,
            &((source.span, source.src()).into()),
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
                match self.def_kind(*def_id) {
                    Some(DefKind::InterfaceSelfPlaceholder) => self
                        .result
                        .interner
                        .intern(Type::InterfaceSelfPlaceholder(*def_id)),

                    _ => {
                        let struct_generics = self
                            .struct_generics
                            .get(def_id)
                            .cloned()
                            .unwrap_or_default();
                        let generic_args: Vec<TypeId> = struct_generics
                            .iter()
                            .map(|&g| self.result.interner.intern(Type::GenericParam(g)))
                            .collect();

                        self.result.interner.intern(Type::Struct {
                            def_id: *def_id,
                            generic_args,
                        })
                    }
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

                    _ => {
                        let struct_generics = self
                            .struct_generics
                            .get(def_id)
                            .cloned()
                            .unwrap_or_default();

                        if !struct_generics.is_empty() && args.len() != struct_generics.len() {
                            let interner = self.interner.borrow();
                            let name = interner.resolve(&self.resolution.defs[def_id].name).into();
                            drop(interner);

                            self.report(TypeError::GenericArgCountMismatch {
                                name,
                                expected: struct_generics.len(),
                                found: args.len(),
                                src: ty.source.src(),
                                span: ty.source.span,
                            });

                            return self.result.interner.error();
                        }

                        self.result.interner.intern(Type::Struct {
                            def_id: *def_id,
                            generic_args: args,
                        })
                    }
                }
            }

            HirTypeKind::Const(inner) => self.lower_hir_type(inner),

            HirTypeKind::SinglePointer(inner) => {
                let is_const = matches!(inner.kind, HirTypeKind::Const(_));
                let inner_ty = self.lower_hir_type(inner);
                self.result.interner.intern(Type::Pointer {
                    inner: inner_ty,
                    is_const,
                })
            }

            HirTypeKind::ManyPointer(inner) => {
                let is_const = matches!(inner.kind, HirTypeKind::Const(_));
                let inner_ty = self.lower_hir_type(inner);
                self.result.interner.intern(Type::ManyPointer {
                    inner: inner_ty,
                    is_const,
                })
            }

            HirTypeKind::Array { element, len } => {
                let elem_ty = self.lower_hir_type(element);
                let is_const = matches!(element.kind, HirTypeKind::Const(_));

                let len_val = len.as_ref().and_then(|expr| self.eval_const_u64(expr));

                if let Some(0) = len_val {
                    self.report(TypeError::EmptyArrayError {
                        src: ty.source.src(),
                        span: ty.source.span,
                    });
                    return self.result.interner.error();
                }

                if len_val.is_some() {
                    self.result.interner.intern(Type::Array {
                        element: elem_ty,
                        len: len_val,
                    })
                } else {
                    self.result.interner.intern(Type::Slice {
                        element: elem_ty,
                        is_const,
                    })
                }
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
        let HirDeclKind::Struct(_) = &decl.kind else {
            return;
        };

        let is_copy = self.struct_implements_by_name(decl.def_id, "Copy");
        let has_explicit_drop = self.struct_implements_by_name(decl.def_id, "Drop");

        if let Some(info) = self.result.struct_info.get_mut(&decl.def_id) {
            info.capabalities = Capabilities {
                is_copy,
                has_explicit_drop,
            };
        }
    }

    fn struct_implements_by_name(&self, struct_def: DefId, iface_name: &str) -> bool {
        let Some(iface_def) = self.interface_registry.get(iface_name) else {
            return false;
        };
        self.resolution.impls.contains_key(&(struct_def, iface_def))
    }

    // > Pass 3

    // Declarations

    fn check_decl_body(&mut self, decl: &HirDecl) {
        match &decl.kind {
            HirDeclKind::Fn(hir_fn) => self.check_fn_body(decl.def_id, hir_fn, None, None),

            HirDeclKind::Struct(s) => {
                let _self_ty = self.result.interner.intern(Type::Struct {
                    def_id: decl.def_id,
                    generic_args: Vec::new(),
                });

                for method in &s.methods {
                    self.check_decl_body_as_method(method, decl.def_id, None);
                }
            }

            HirDeclKind::Interface(i) => {
                for method in &i.methods {
                    if let HirDeclKind::Fn(f) = &method.kind {
                        self.check_fn_body(method.def_id, f, None, None);
                    }
                }
            }

            HirDeclKind::Implement(imp) => {
                if let Some(object_def) = imp.object {
                    let _self_ty = self.result.interner.intern(Type::Struct {
                        def_id: object_def,
                        generic_args: Vec::new(),
                    });

                    for method in &imp.methods {
                        self.check_decl_body_as_method(method, object_def, imp.interface);
                    }
                } else {
                    for method in &imp.methods {
                        if let HirDeclKind::Fn(f) = &method.kind {
                            self.check_fn_body(method.def_id, f, None, None);
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

    fn check_decl_body_as_method(
        &mut self,
        method: &HirDecl,
        struct_def: DefId,
        iface_def: Option<DefId>,
    ) {
        if let HirDeclKind::Fn(f) = &method.kind {
            self.check_fn_body(method.def_id, f, Some(struct_def), iface_def);
        }
    }

    fn check_fn_body(
        &mut self,
        def_id: DefId,
        hir_fn: &HirFn,
        struct_def: Option<DefId>,
        iface_def: Option<DefId>,
    ) {
        let Some(body) = &hir_fn.body else {
            return;
        };

        let sig = self
            .fn_sigs
            .get(&def_id)
            .expect("unregistered signature, wtf");

        let self_type = struct_def.map(|sd| {
            let struct_generics = self.struct_generics.get(&sd).cloned().unwrap_or_default();

            let generic_args: Vec<TypeId> = if let Some(iface) = iface_def
                && let Some(mapping) = self.impl_generic_to_struct_generic.get(&(sd, iface))
            {
                let reverse: HashMap<DefId, DefId> =
                    mapping.iter().map(|(&k, &v)| (v, k)).collect();

                struct_generics
                    .iter()
                    .map(|g| {
                        let target = reverse.get(g).copied().unwrap_or(*g);
                        self.result.interner.intern(Type::GenericParam(target))
                    })
                    .collect()
            } else {
                struct_generics
                    .iter()
                    .map(|&g| self.result.interner.intern(Type::GenericParam(g)))
                    .collect()
            };

            let base_struct_ty = self.result.interner.intern(Type::Struct {
                def_id: sd,
                generic_args,
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
            struct_def,
            self_type,
            generic_bindings,
            generic_bounds,
            loop_depth: 0,
        });

        match &body.kind {
            HirStmtKind::Expr(block_expr) => {
                if let HirExprKind::Block { stmts, trailing } = &block_expr.kind {
                    let ty = self.check_block(stmts, trailing, Some(sig.ret), &block_expr.source);
                    self.result.record_expr_type(block_expr.id, ty);
                } else {
                    self.check_stmt(body);
                }
            }

            _ => self.check_stmt(body),
        };

        self.ctx.pop_fn();
    }

    // Statements

    fn check_stmt(&mut self, stmt: &HirStmt) {
        match &stmt.kind {
            HirStmtKind::Let {
                def_id,
                name: _,
                explicit_type,
                value,
                is_const,
            } => {
                let declared = explicit_type
                    .as_ref()
                    .map(|t| self.lower_hir_type_with_const(t));

                let declared_ty = declared.map(|(ty, _)| ty);
                let _declared_const = declared.map(|(_, c)| c).unwrap_or(false);

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
                self.result.expr_types.insert(stmt.id, final_ty);

                self.result.const_bindings.insert(*def_id, *is_const);
            }

            HirStmtKind::Assign { object, value } => {
                let prev_expect = self.expect_assign_interface;
                self.expect_assign_interface = true;

                let obj_ty = self.synth_expr(object);
                self.expect_assign_interface = prev_expect;

                self.check_expr(value, obj_ty, false);
                self.check_not_const_target(object);
            }

            HirStmtKind::CompoundAssign { object, value, op } => {
                let prev_expect = self.expect_assign_interface;
                self.expect_assign_interface = true;

                let obj_ty = self.synth_expr(object);
                self.expect_assign_interface = prev_expect;

                let value_ty = self.synth_expr(value);
                self.check_binary_op(*op, obj_ty, value_ty, object.id, stmt.source.clone());
                self.check_not_const_target(object);
            }

            HirStmtKind::While { condition, block } => {
                let bool_ty = self.result.interner.builtin(BuiltinType::bool);
                self.check_expr(condition, bool_ty, false);

                self.ctx.enter_loop();
                self.check_stmt(block);
                self.ctx.exit_loop();
            }

            HirStmtKind::For {
                def_id,
                varname: _,
                iterator,
                block,
            } => {
                let iter_ty = self.synth_expr(iterator);
                let elem_ty = match self.result.interner.get(iter_ty).clone() {
                    Type::IntLiteral => self.result.interner.builtin(DEFAULT_INT_LITERAL),
                    Type::Builtin(b) if coerce::builtin_is_integer(b) => iter_ty,
                    Type::Array { element, .. } => element,
                    Type::Slice { element, .. } => element,
                    Type::Error => self.result.interner.error(),
                    _ => {
                        self.report(TypeError::NotIterable {
                            child_type: self.display_type(iter_ty).into(),
                            src: iterator.source.src(),
                            span: iterator.source.span,
                        });
                        self.result.interner.error()
                    }
                };

                self.result.expr_types.insert(stmt.id, elem_ty);
                self.result.def_types.insert(*def_id, elem_ty);

                self.ctx.enter_loop();
                self.check_stmt(block);
                self.ctx.exit_loop();
            }

            HirStmtKind::Break => {
                if !self.ctx.in_loop() {
                    self.report(TypeError::BreakOutsideLoop {
                        src: stmt.source.src(),
                        span: stmt.source.span,
                    });
                }
            }
            HirStmtKind::Continue => {
                if !self.ctx.in_loop() {
                    self.report(TypeError::ContinueOutsideLoop {
                        src: stmt.source.src(),
                        span: stmt.source.span,
                    });
                }
            }

            HirStmtKind::Return { value } => {
                let expected = self.ctx.current().return_type;

                match value {
                    Some(v) => {
                        self.check_expr(v, expected, false);
                    }

                    None => {
                        let void = self.result.interner.void();
                        if !try_coerce(&mut self.result.interner, void, expected).is_ok() {
                            self.report(TypeError::Mismatch {
                                expected: self.display_type(expected).into(),
                                found: self.display_type(void).into(),
                                src: stmt.source.src(),
                                span: stmt.source.span,
                            });
                        }
                    }
                }
            }

            HirStmtKind::Expr(expr) => {
                self.synth_expr(expr);
            }

            HirStmtKind::Error => {}
        }
    }

    fn check_stmt_as_block_value(&mut self, stmt: &HirStmt, expected: Option<TypeId>) -> TypeId {
        match &stmt.kind {
            HirStmtKind::Expr(block_expr) => {
                if let HirExprKind::Block { stmts, trailing } = &block_expr.kind {
                    let ty = self.check_block(stmts, trailing, expected, &block_expr.source);
                    self.result.record_expr_type(block_expr.id, ty);
                    ty
                } else {
                    self.check_stmt(stmt);
                    self.result.interner.void()
                }
            }
            _ => {
                self.check_stmt(stmt);
                self.result.interner.void()
            }
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
                Literal::String(str_lit) => {
                    let interner = self.interner.borrow();
                    let str_resolved = interner.resolve(str_lit).to_string();
                    drop(interner);

                    let char_ty = self.result.interner.builtin(BuiltinType::char);
                    self.result.interner.intern(Type::Array {
                        element: char_ty,
                        len: Some(str_resolved.len() as u64 + 1), // + 1 for null-terminator
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
                .current()
                .self_type
                .unwrap_or_else(|| self.lookup_def_type(*def_id, expr.source.clone())),

            HirExprKind::MacroCall { kind, args } => {
                self.check_macro_call(expr.id, *kind, args, expr.source.clone())
            }

            HirExprKind::FieldAccess { object, field } => {
                self.check_field_access(expr.id, object, field)
            }

            HirExprKind::StructInit {
                ty,
                fields,
                generic_args,
            } => {
                let (ty_def, ty_span) = *ty;
                self.check_struct_init(ty_def, generic_args, fields, ty_span, &expr.source)
            }

            HirExprKind::Binary { lhs, rhs, op } => {
                let lhs_ty = self.synth_expr(lhs);
                let rhs_ty = self.synth_expr(rhs);

                self.check_binary_op(*op, lhs_ty, rhs_ty, expr.id, expr.source.clone())
            }

            HirExprKind::Unary { expr, op } => {
                let inner_ty = self.synth_expr(expr);
                self.check_unary_op(*op, inner_ty, expr.id, expr.source.clone())
            }

            HirExprKind::Call {
                callee,
                args,
                generic_args,
            } => self.check_call(expr.id, callee, args, generic_args, expr.source.clone()),

            HirExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let bool_ty = self.result.interner.builtin(BuiltinType::bool);
                self.check_expr(condition, bool_ty, false);

                let then_ty = self.check_stmt_as_block_value(then_block, None);
                match else_block {
                    Some(else_b) => {
                        let else_ty = self.check_stmt_as_block_value(else_b, None);
                        self.unify_branches(then_ty, else_ty, expr.source.clone())
                    }
                    None => self.result.interner.void(),
                }
            }

            HirExprKind::Block { stmts, trailing } => {
                self.check_block(stmts, trailing, None, &expr.source)
            }

            HirExprKind::Switch => unreachable!(),

            HirExprKind::SliceAccess { object, index } => {
                let obj_ty = self.synth_expr(object);
                let usize_ty = self.result.interner.builtin(BuiltinType::usize);
                let index_ty = self.check_expr(index, usize_ty, false);

                match self.result.interner.get(obj_ty).clone() {
                    Type::Array { element, .. } => element,
                    Type::Slice { element, .. } => element,
                    Type::ManyPointer { inner, .. } => inner,
                    Type::Struct {
                        def_id,
                        generic_args,
                    } => self.check_slice_access_on_struct(
                        def_id,
                        &generic_args,
                        index_ty,
                        expr.id,
                        &expr.source,
                    ),
                    Type::Error => self.result.interner.error(),
                    _ => {
                        self.report(TypeError::NotIndexable {
                            child_type: self.display_type(obj_ty).into(),
                            src: object.source.src(),
                            span: object.source.span,
                        });
                        self.result.interner.error()
                    }
                }
            }

            HirExprKind::ArrayInit { elements } => {
                if elements.is_empty() {
                    self.report(TypeError::EmptyArrayError {
                        src: expr.source.src(),
                        span: expr.source.span,
                    });
                    return self.result.interner.error();
                }

                let first_ty = self.synth_expr(&elements[0]);
                let first_ty = self.default_literal(first_ty);

                for el in &elements[1..] {
                    self.check_expr(el, first_ty, false);
                }

                self.result.interner.intern(Type::Array {
                    element: first_ty,
                    len: Some(elements.len() as u64),
                })
            }

            HirExprKind::Type(_) => self.result.interner.error(),
            HirExprKind::Error => self.result.interner.error(),
        }
    }

    fn check_block(
        &mut self,
        stmts: &[Rc<HirStmt>],
        trailing: &Option<Rc<HirExpr>>,
        expected: Option<TypeId>,
        source: &Source,
    ) -> TypeId {
        for stmt in stmts {
            self.check_stmt(stmt);
        }

        match trailing {
            Some(expr) => match expected {
                Some(exp) => self.check_expr(expr, exp, false),
                None => self.synth_expr(expr),
            },
            None => {
                let diverges = stmts.last().is_some_and(|s| self.stmt_diverges(s));
                let actual = if diverges {
                    self.result.interner.never()
                } else {
                    self.result.interner.void()
                };

                match expected {
                    Some(exp) => match try_coerce(&mut self.result.interner, actual, exp) {
                        CoerceResult::Fail => {
                            self.report(TypeError::Mismatch {
                                expected: self.display_type(exp).into(),
                                found: self.display_type(actual).into(),
                                src: source.src(),
                                span: source.span,
                            });

                            actual
                        }
                        _ => exp,
                    },
                    None => actual,
                }
            }
        }
    }

    fn stmt_diverges(&self, stmt: &HirStmt) -> bool {
        match &stmt.kind {
            HirStmtKind::Return { .. } | HirStmtKind::Break | HirStmtKind::Continue => true,
            HirStmtKind::Expr(e) => self.expr_diverges(e),
            _ => false,
        }
    }

    fn expr_diverges(&self, expr: &HirExpr) -> bool {
        match &expr.kind {
            HirExprKind::If {
                then_block,
                else_block: Some(else_b),
                ..
            } => self.stmt_diverges(then_block) && self.stmt_diverges(else_b),
            HirExprKind::Block { stmts, trailing } => {
                stmts.iter().any(|s| self.stmt_diverges(s))
                    || trailing.as_ref().is_some_and(|t| self.expr_diverges(t))
            }
            _ => false,
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

            HirMacroKind::Panic => {
                self.check_format_macro(call_id, args, source);
                self.result.interner.never()
            }

            HirMacroKind::Unreachable | HirMacroKind::Todo => self.result.interner.never(),

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

                let implements_iface = self.type_implements_debug(ty);

                if !implements_iface {
                    self.report(TypeError::InterfaceNotImplemented {
                        name: "Debug".into(),
                        ty_name: self.display_type(ty).into(),
                        src: source.src(),
                        span: source.span,
                    });
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

                let value_ty = self.synth_expr(&args[1]);
                let value_ty = self.default_literal(value_ty);

                if !coerce::verify_cast(&mut self.result.interner, value_ty, target_ty) {
                    self.report(TypeError::InvalidCast {
                        from: self.display_type(value_ty).into(),
                        to: self.display_type(target_ty).into(),
                        src: source.src(),
                        span: source.span,
                    });
                }

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

    fn type_implements_display(&self, ty: TypeId) -> bool {
        match self.result.interner.get(ty).clone() {
            Type::Struct { def_id, .. } => self.struct_implements_by_name(def_id, "Display"),
            Type::IntLiteral | Type::FloatLiteral => true,
            Type::Never | Type::Error => true,
            Type::Enum { .. } => true,

            Type::Array { element, .. } | Type::Slice { element, .. } => {
                self.type_implements_display(element)
            }

            Type::GenericParam(g) => {
                let cur = self.ctx.current();

                let Some(bounds) = cur.generic_bounds.get(&g) else {
                    return false;
                };
                let Some(iface) = self.interface_registry.get("Display") else {
                    return false;
                };

                bounds.contains(&iface)
            }

            Type::Builtin(_) => true,

            _ => false,
        }
    }

    fn type_implements_debug(&self, ty: TypeId) -> bool {
        match self.result.interner.get(ty).clone() {
            Type::Struct { def_id, .. } => self.struct_implements_by_name(def_id, "Debug"),
            Type::IntLiteral | Type::FloatLiteral => true,
            Type::Never | Type::Error => true,
            Type::Enum { .. } => true,

            Type::Array { element, .. } | Type::Slice { element, .. } => {
                self.type_implements_debug(element)
            }

            Type::GenericParam(g) => {
                let cur = self.ctx.current();

                let Some(bounds) = cur.generic_bounds.get(&g) else {
                    return false;
                };
                let Some(iface) = self.interface_registry.get("Debug") else {
                    return false;
                };

                bounds.contains(&iface)
            }

            Type::Builtin(_) => true,
            Type::Pointer { .. } => true,

            _ => false,
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

    fn check_format_arg(&mut self, spec: FormatSpec, arg_ty: TypeId, source: Source) {
        match spec {
            FormatSpec::Display => {
                let implements_iface = self.type_implements_display(arg_ty);

                if !implements_iface {
                    self.report(TypeError::InterfaceNotImplemented {
                        name: "Display".into(),
                        ty_name: self.display_type(arg_ty).into(),
                        src: source.src(),
                        span: source.span,
                    });
                };
            }

            FormatSpec::Debug => {
                let implements_iface = self.type_implements_debug(arg_ty);

                if !implements_iface {
                    self.report(TypeError::InterfaceNotImplemented {
                        name: "Debug".into(),
                        ty_name: self.display_type(arg_ty).into(),
                        src: source.src(),
                        span: source.span,
                    });
                };
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
                    }
                }
            }

            FormatSpec::Float { .. } => match self.result.interner.get(arg_ty) {
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
                }
            },
        }
    }

    // << Macros

    fn check_field_access(
        &mut self,
        id: HirId,
        object: &HirExpr,
        field: &(Spur, SourceSpan),
    ) -> TypeId {
        let (field_name, field_span) = *field;

        if let HirExprKind::VarRef(referenced_def) = &object.kind
            && matches!(self.def_kind(*referenced_def), Some(DefKind::Enum))
        {
            return self.check_enum_variant_access(
                id,
                *referenced_def,
                field_name,
                field_span,
                &object.source,
            );
        }

        let obj_ty = self.synth_expr(object);

        // -----------| Hard coded piece of shit section |-----------
        // > What is this for?
        // Answer: for arrays and slices builtin `.len` field

        {
            let mut interner = self.interner.borrow_mut();
            if field_name == interner.get_or_intern("len")
                && matches!(
                    self.result.interner.get(obj_ty),
                    Type::Array { .. } | Type::Slice { .. }
                )
            {
                return self.result.interner.builtin(BuiltinType::usize);
            }
        }

        // ----------------------------------------------------------

        let (struct_def, struct_generic_args) = match self.result.interner.get(obj_ty).clone() {
            Type::Struct {
                def_id,
                generic_args,
                ..
            } => (def_id, generic_args),
            Type::Pointer { inner, .. } => match self.result.interner.get(inner).clone() {
                Type::Struct {
                    def_id,
                    generic_args,
                } => (def_id, generic_args),
                Type::Error => return self.result.interner.error(),
                _ => {
                    self.report(TypeError::NotAStruct {
                        provided: self.display_type(obj_ty).into(),
                        src: object.source.src(),
                        span: field_span,
                    });
                    return self.result.interner.error();
                }
            },

            Type::Error => return self.result.interner.error(),

            _ => {
                self.report(TypeError::NotAStruct {
                    provided: self.display_type(obj_ty).into(),
                    src: object.source.src(),
                    span: field_span,
                });
                return self.result.interner.error();
            }
        };

        let Some(info) = self.result.struct_info.get(&struct_def).cloned() else {
            return self.result.interner.error();
        };

        match info.fields.iter().find(|f| f.name == field_name) {
            Some(f) => {
                self.result.field_resolutions.insert(id, f.field_def);

                let same_struct = self
                    .ctx
                    .current()
                    .struct_def
                    .map(|def| def == f.struct_def)
                    .unwrap_or(false);

                let struct_generics = self
                    .struct_generics
                    .get(&struct_def)
                    .cloned()
                    .unwrap_or_default();

                if !f.is_pub && !same_struct {
                    let interner = self.interner.borrow();
                    let name = interner.resolve(&field_name).into();

                    drop(interner);

                    self.report(TypeError::PrivateItemNotAccessible {
                        name,
                        src: object.source.src(),
                        span: field_span,
                    });
                }

                let bindings: HashMap<DefId, TypeId> = struct_generics
                    .iter()
                    .copied()
                    .zip(struct_generic_args.iter().copied())
                    .collect();

                self.substitute_generics(f.field_ty, &bindings)
            }
            None => {
                let interner = self.interner.borrow();

                let struct_name_id = self.resolution.defs.get(&struct_def).expect("wtf").name;

                let struct_name = interner.resolve(&struct_name_id).into();
                let field_name = interner.resolve(&field_name).into();

                drop(interner);

                self.report(TypeError::UnknownField {
                    struct_name,
                    field: field_name,
                    src: object.source.src(),
                    span: field_span,
                });
                self.result.interner.error()
            }
        }
    }

    fn check_struct_init(
        &mut self,
        ty_def: Option<DefId>,
        ty_generic_args: &[Rc<HirTypeExpr>],
        fields: &[HirFieldInit],
        ty_span: SourceSpan,
        init_source: &Source,
    ) -> TypeId {
        let Some(def_id) = ty_def else {
            for f in fields {
                self.synth_expr(&f.value);
            }
            return self.result.interner.error();
        };

        let struct_generics = self
            .struct_generics
            .get(&def_id)
            .cloned()
            .unwrap_or_default();

        let field_table: Vec<StructFieldInfo> = self
            .result
            .struct_info
            .get(&def_id)
            .map(|info| info.fields.clone())
            .unwrap_or_default();

        let mut bindings: HashMap<DefId, TypeId> = HashMap::new();
        if !ty_generic_args.is_empty() {
            if ty_generic_args.len() != struct_generics.len() {
                let interner = self.interner.borrow();

                let name = interner.resolve(&self.resolution.defs[&def_id].name).into();

                drop(interner);

                self.report(TypeError::GenericArgCountMismatch {
                    name,
                    expected: struct_generics.len(),
                    found: ty_generic_args.len(),
                    src: init_source.src(),
                    span: ty_span,
                });
            }

            for (g, explicit) in struct_generics.iter().zip(ty_generic_args.iter()) {
                let ty = self.lower_hir_type(explicit);
                bindings.insert(*g, ty);
            }
        }

        let expected_names: HashSet<Spur> = field_table.iter().map(|info| info.name).collect();
        let mut provided_names: HashSet<Spur> = HashSet::with_capacity(fields.len());
        let mut field_value_types: HashMap<Spur, TypeId> = HashMap::with_capacity(fields.len());

        for f in fields {
            provided_names.insert(f.name);

            let Some(info) = field_table.iter().find(|i| i.name == f.name) else {
                let interner = self.interner.borrow();
                let field = interner.resolve(&f.name).into();
                let struct_name = interner.resolve(&self.resolution.defs[&def_id].name).into();
                drop(interner);

                self.report(TypeError::UnknownField {
                    field,
                    struct_name,
                    src: init_source.src(),
                    span: f.span,
                });
                self.synth_expr(&f.value);
                continue;
            };

            let value_ty = self.synth_expr(&f.value);
            let value_ty = self.default_literal(value_ty);
            field_value_types.insert(f.name, value_ty);

            if self.type_contains_generic(info.field_ty) {
                self.unify_for_inference(
                    info.field_ty,
                    value_ty,
                    &mut bindings,
                    (f.span, init_source.src()).into(),
                );
            }
        }

        for g in &struct_generics {
            if !bindings.contains_key(g) {
                let interner = self.interner.borrow();
                let generic_name = interner.resolve(&self.resolution.defs[g].name).into();
                drop(interner);

                self.report(TypeError::CannotInferGeneric {
                    generic_name,
                    src: init_source.src(),
                    span: init_source.span,
                });

                bindings.insert(*g, self.result.interner.error());
            }
        }

        let resolved_args: Vec<TypeId> = struct_generics.iter().map(|g| bindings[g]).collect();
        let struct_ty = self.result.interner.intern(Type::Struct {
            def_id,
            generic_args: resolved_args,
        });

        for f in fields {
            let Some(info) = field_table.iter().find(|i| i.name == f.name).cloned() else {
                continue;
            };

            let expected_ty = self.substitute_generics(info.field_ty, &bindings);
            let actual_ty = field_value_types
                .get(&f.name)
                .copied()
                .unwrap_or(expected_ty);

            self.coerce_or_error(
                actual_ty,
                expected_ty,
                f.value.source.clone(),
                f.value.id,
                false,
            );
        }

        let missing: Vec<Spur> = expected_names
            .difference(&provided_names)
            .copied()
            .collect();

        if !missing.is_empty() {
            let interner = self.interner.borrow();
            let missing_stringified: Vec<String> = missing
                .iter()
                .map(|x| format!("`{}`", interner.resolve(x)))
                .collect();
            drop(interner);

            let fields = missing_stringified.join(", ").into();

            self.report(TypeError::MissingFields {
                struct_name: self.display_type(struct_ty).into(),
                fields,
                src: init_source.src(),
                span: init_source.span,
            });
        }

        struct_ty
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

            HirExprKind::Block { stmts, trailing } => {
                let ty = self.check_block(stmts, trailing, Some(expected), &expr.source);
                self.result.record_expr_type(expr.id, ty);
                return ty;
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
        match try_coerce(&mut self.result.interner, actual, expected) {
            CoerceResult::Identity => actual,
            CoerceResult::ErrorRecovery => expected,

            CoerceResult::PinLiteral
            | CoerceResult::AddConst
            | CoerceResult::ArrayToSlice
            | CoerceResult::ArrayToManyPointer
            | CoerceResult::NeverCoercion
            | CoerceResult::VoidPtrCoercion => {
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
            self.report(TypeError::AssignToConst {
                src: target.source.src(),
                span: target.source.span,
            })
        }
    }

    fn unify_branches(&mut self, a: TypeId, b: TypeId, source: Source) -> TypeId {
        if a == b {
            return a;
        }

        if matches!(self.result.interner.get(a), Type::Never) {
            return b;
        }
        if matches!(self.result.interner.get(b), Type::Never) {
            return a;
        }

        if try_coerce(&mut self.result.interner, b, a).is_ok() {
            return a;
        }
        if try_coerce(&mut self.result.interner, a, b).is_ok() {
            return b;
        }

        self.report(TypeError::Mismatch {
            expected: self.display_type(a).into(),
            found: self.display_type(b).into(),
            src: source.src(),
            span: source.span,
        });
        self.result.interner.error()
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
        let sig_is_va = sig.is_variadic;

        let sig_params = if sig_is_va {
            sig_params[..sig_params.len().saturating_sub(1)].to_vec()
        } else {
            sig_params
        };

        self.result
            .record_expr_type(callee.id, self.result.def_types[&def_id]);

        let count_condition = if sig_is_va {
            args.len() >= sig_params.len()
        } else {
            args.len() == sig_params.len()
        };

        if !count_condition {
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

        for (idx, (param_ty, arg)) in sig_params.iter().zip(args.iter()).enumerate() {
            if idx > sig_params.len() - 1 {
                // variadic args
                continue;
            }

            self.infer_or_check_arg(*param_ty, arg, &mut bindings, source.clone());
        }

        for g in &sig_generics {
            if !bindings.contains_key(g) {
                let interner = self.interner.borrow();
                let generic_name = interner.resolve(&self.resolution.defs[g].name).into();
                drop(interner);

                self.report(TypeError::CannotInferGeneric {
                    generic_name,
                    src: source.src(),
                    span: source.span,
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

    fn check_method_visibility(
        &mut self,
        method_def_id: DefId,
        owner_struct: DefId,
        caller_expr: &HirExpr,
        source: &Source,
    ) {
        let _ = caller_expr;

        let Some(sig) = self.fn_sigs.get(&method_def_id) else {
            return;
        };
        if sig.is_pub {
            return;
        }

        let Some(_method_info) = self.resolution.defs.get(&method_def_id) else {
            return;
        };

        let current_struct = self.ctx.current().struct_def;

        if Some(owner_struct) != current_struct {
            let name = self.def_name(method_def_id).unwrap_or_default().into();

            self.report(TypeError::PrivateItemNotAccessible {
                name,
                src: source.src(),
                span: source.span,
            });
        }
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

        // If object is a direct ref to struct type - associated fn call
        if let HirExprKind::VarRef(referenced_def) = &object.kind
            && matches!(self.def_kind(*referenced_def), Some(DefKind::Struct))
        {
            return self.check_associated_fn_call(
                (call_id, object),
                *referenced_def,
                (field_name, field_span),
                args,
                explicit_generic_args,
                &source,
            );
        }

        // Otherwise it is instance call
        let obj_ty = self.synth_expr(object);

        let (struct_def, struct_generic_args, obj_is_ptr, ptr_is_const) =
            match self.result.interner.get(obj_ty).clone() {
                Type::Struct {
                    def_id,
                    generic_args,
                } => (def_id, generic_args, false, false),
                Type::Pointer { inner, is_const } => {
                    match self.result.interner.get(inner).clone() {
                        Type::Struct {
                            def_id,
                            generic_args,
                        } => (def_id, generic_args, true, is_const),
                        _ => return None,
                    }
                }
                _ => return None,
            };

        let is_field = self
            .result
            .struct_info
            .get(&struct_def)
            .map(|info| info.fields.iter().any(|f| f.name == field_name))
            .unwrap_or(false);

        if is_field {
            return None;
        }

        let method_def_id = *self.struct_methods.get(&struct_def)?.get(&field_name)?;
        let sig = &self.fn_sigs[&method_def_id];
        let self_mode = sig.self_mode;

        let sig_params = sig.params.clone();
        let sig_ret = sig.ret;
        let sig_generics = sig.generics.clone();

        let _ = self_mode?; // wth i didn't even knew this exist in Rust

        self.check_method_visibility(
            method_def_id,
            struct_def,
            object,
            &(field_span, source.src()).into(),
        );

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

        let struct_generics = self
            .struct_generics
            .get(&struct_def)
            .cloned()
            .unwrap_or_default();
        let mut bindings: HashMap<DefId, TypeId> = struct_generics
            .iter()
            .copied()
            .zip(struct_generic_args.iter().copied())
            .collect();

        if let Some(&owning_iface) = self.method_owning_interface.get(&method_def_id)
            && let Some(impl_to_struct) = self
                .impl_generic_to_struct_generic
                .get(&(struct_def, owning_iface))
        {
            for (&impl_g, &struct_g) in impl_to_struct {
                if let Some(&concrete) = bindings.get(&struct_g) {
                    bindings.insert(impl_g, concrete);
                }
            }
        }

        for (g, explicit) in sig_generics.iter().zip(explicit_generic_args.iter()) {
            let ty = self.lower_hir_type(explicit);
            bindings.insert(*g, ty);
        }

        for (param_ty, arg) in sig_params.iter().zip(args.iter()) {
            self.infer_or_check_arg(*param_ty, arg, &mut bindings, source.clone());
        }

        for g in &sig_generics {
            if !bindings.contains_key(g) {
                let interner = self.interner.borrow();
                let generic_name = interner.resolve(&self.resolution.defs[g].name).into();
                drop(interner);

                self.report(TypeError::CannotInferGeneric {
                    generic_name,
                    src: source.src(),
                    span: source.span,
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

    fn check_enum_variant_access(
        &mut self,
        id: HirId,
        enum_def: DefId,
        field_name: Spur,
        field_span: SourceSpan,
        source: &Source,
    ) -> TypeId {
        let Some(variant_defs) = self.enum_variants.get(&enum_def) else {
            return self.result.interner.error();
        };

        let variant_def_id = variant_defs.iter().find(|&v| {
            self.resolution
                .defs
                .get(v)
                .map(|info| info.name == field_name)
                .unwrap_or(false)
        });

        let enum_ty = self.result.interner.intern(Type::Enum { def_id: enum_def });

        match variant_def_id {
            Some(&variant_def) => {
                self.result.field_resolutions.insert(id, variant_def);
                enum_ty
            }

            None => {
                let interner = self.interner.borrow();
                let field_name = interner.resolve(&field_name).into();
                drop(interner);

                self.report(TypeError::UnknownEnumVariant {
                    name: self.display_type(enum_ty).into(),
                    variant: field_name,
                    src: source.src(),
                    span: field_span,
                });

                self.result.interner.error()
            }
        }
    }

    fn check_associated_fn_call(
        &mut self,
        caller: (HirId, &HirExpr),
        struct_def: DefId,
        field: (Spur, SourceSpan),
        args: &[Rc<HirExpr>],
        explicit_generic_args: &[Rc<HirTypeExpr>],
        source: &Source,
    ) -> Option<TypeId> {
        let (call_id, caller_expr) = caller;
        let (field_name, field_span) = field;

        let method_def_id = *self.struct_methods.get(&struct_def)?.get(&field_name)?;
        let self_mode = self.fn_sigs[&method_def_id].self_mode;

        if self_mode.is_some() {
            self.report(TypeError::AssociatedCallOnInstaneMethod {
                src: source.src(),
                span: field_span,
            });
            return Some(self.result.interner.error());
        }

        self.check_method_visibility(
            method_def_id,
            struct_def,
            caller_expr,
            &(field_span, source.src()).into(),
        );

        let sig_params = self.fn_sigs[&method_def_id].params.clone();
        let sig_ret = self.fn_sigs[&method_def_id].ret;
        let sig_generics = self.fn_sigs[&method_def_id].generics.clone();

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
                let interner = self.interner.borrow();
                let generic_name = interner.resolve(&self.resolution.defs[g].name).into();
                drop(interner);

                self.report(TypeError::CannotInferGeneric {
                    generic_name,
                    src: source.src(),
                    span: source.span,
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
            Type::ManyPointer { inner, .. } => self.type_contains_generic(*inner),
            Type::Array { element, .. } | Type::Slice { element, .. } => {
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
        match (
            self.result.interner.get(param_ty).clone(),
            self.result.interner.get(arg_ty).clone(),
        ) {
            (Type::GenericParam(g), _) => match bindings.get(&g) {
                Some(&existing)
                    if existing != arg_ty
                        && !try_coerce(&mut self.result.interner, arg_ty, existing).is_ok() =>
                {
                    self.report(TypeError::GenericConflict {
                        param: self.display_type(param_ty).into(),
                        first: self.display_type(existing).into(),
                        second: self.display_type(arg_ty).into(),
                        src: source.src(),
                        span: source.span,
                    });
                }

                Some(_) => {}
                None => {
                    bindings.insert(g, arg_ty);
                }
            },

            (Type::Pointer { inner: pinner, .. }, Type::Pointer { inner: ainner, .. })
            | (Type::ManyPointer { inner: pinner, .. }, Type::ManyPointer { inner: ainner, .. }) => {
                self.unify_for_inference(pinner, ainner, bindings, source);
            }

            (Type::Array { element: pelem, .. }, Type::Array { element: aelem, .. })
            | (Type::Slice { element: pelem, .. }, Type::Slice { element: aelem, .. })
            | (Type::Array { element: pelem, .. }, Type::Slice { element: aelem, .. })
            | (Type::Slice { element: pelem, .. }, Type::Array { element: aelem, .. }) => {
                self.unify_for_inference(pelem, aelem, bindings, source);
            }

            (
                Type::Struct {
                    def_id: pd,
                    generic_args: pa,
                },
                Type::Struct {
                    def_id: ad,
                    generic_args: aa,
                },
            ) if pd == ad => {
                for (p, a) in pa.iter().zip(aa.iter()) {
                    self.unify_for_inference(*p, *a, bindings, source.clone());
                }
            }

            (
                Type::Fn {
                    params: pp,
                    ret: pr,
                },
                Type::Fn {
                    params: ap,
                    ret: ar,
                },
            ) if pp.len() == ap.len() => {
                for (p, a) in pp.iter().zip(ap.iter()) {
                    self.unify_for_inference(*p, *a, bindings, source.clone());
                }
                self.unify_for_inference(pr, ar, bindings, source);
            }

            _ => {}
        }
    }

    fn builtin_interface_names(b: BuiltinType) -> &'static [&'static str] {
        use BuiltinType::*;

        match b {
            i8 | i16 | i32 | i64 | isize => &[
                "Display", "Debug", "Eq", "Add", "Sub", "Mul", "Div", "Mod", "BitAnd", "BitOr",
                "BitXor", "Shl", "Shr", "BitNot", "Neg",
            ],

            u8 | u16 | u32 | u64 | usize => &[
                "Display", "Debug", "Eq", "Add", "Sub", "Mul", "Div", "Mod", "BitAnd", "BitOr",
                "BitXor", "Shl", "Shr", "BitNot",
            ],

            f32 | f64 => &["Display", "Debug", "Eq", "Add", "Sub", "Mul", "Div", "Neg"],

            bool => &["Display", "Debug", "Eq", "Not"],

            char => &["Display", "Debug", "Eq"],

            void => &[],
        }
    }

    fn enum_interface_names() -> &'static [&'static str] {
        &["Display", "Debug", "Eq"]
    }

    fn type_satisfies_interface(&self, ty: TypeId, iface_def: DefId) -> bool {
        match self.result.interner.get(ty).clone() {
            Type::Error => true,

            Type::Builtin(b) => match self.def_name(iface_def) {
                Some(name) => Self::builtin_interface_names(b).contains(&name.as_str()),
                None => false,
            },

            Type::IntLiteral => match self.def_name(iface_def) {
                Some(name) => {
                    Self::builtin_interface_names(DEFAULT_INT_LITERAL).contains(&name.as_str())
                }
                None => false,
            },

            Type::FloatLiteral => match self.def_name(iface_def) {
                Some(name) => {
                    Self::builtin_interface_names(DEFAULT_FLOAT_LITERAL).contains(&name.as_str())
                }
                None => false,
            },

            Type::Enum { .. } => match self.def_name(iface_def) {
                Some(name) => Self::enum_interface_names().contains(&name.as_str()),
                None => false,
            },

            Type::Struct { def_id, .. } => self.resolution.impls.contains_key(&(def_id, iface_def)),

            _ => false,
        }
    }

    fn substitute_generics(&mut self, ty: TypeId, bindings: &HashMap<DefId, TypeId>) -> TypeId {
        zeen_types::substitute_generics(&mut self.result.interner, ty, bindings)
    }

    fn substitute_self(&mut self, ty: TypeId, self_ty: TypeId) -> TypeId {
        match self.result.interner.get(ty).clone() {
            Type::InterfaceSelfPlaceholder(_) => self_ty,

            Type::Pointer { inner, is_const } => {
                let new_inner = self.substitute_self(inner, self_ty);
                if new_inner == inner {
                    ty
                } else {
                    self.result.interner.intern(Type::Pointer {
                        inner: new_inner,
                        is_const,
                    })
                }
            }

            Type::Array { element, len } => {
                let new_elem = self.substitute_self(element, self_ty);
                if new_elem == element {
                    ty
                } else {
                    self.result.interner.intern(Type::Array {
                        element: new_elem,
                        len,
                    })
                }
            }

            Type::Slice { element, is_const } => {
                let new_elem = self.substitute_self(element, self_ty);
                if new_elem == element {
                    ty
                } else {
                    self.result.interner.intern(Type::Slice {
                        element: new_elem,
                        is_const,
                    })
                }
            }

            Type::Struct {
                def_id,
                generic_args,
            } => {
                let new_args: Vec<TypeId> = generic_args
                    .iter()
                    .map(|a| self.substitute_self(*a, self_ty))
                    .collect();
                if new_args == generic_args {
                    ty
                } else {
                    self.result.interner.intern(Type::Struct {
                        def_id,
                        generic_args: new_args,
                    })
                }
            }

            Type::Fn { params, ret } => {
                let new_params: Vec<TypeId> = params
                    .iter()
                    .map(|p| self.substitute_self(*p, self_ty))
                    .collect();
                let new_ret = self.substitute_self(ret, self_ty);
                if new_params == params && new_ret == ret {
                    ty
                } else {
                    self.result.interner.intern(Type::Fn {
                        params: new_params,
                        ret: new_ret,
                    })
                }
            }

            _ => ty,
        }
    }

    // i'm really sorry 😭
    #[allow(clippy::too_many_arguments)]
    fn call_interface_method(
        &mut self,
        struct_def: DefId,
        struct_generic_args: &[TypeId],
        iface_name: &str,
        method_name: &str,
        explicit_args: &[TypeId],
        receiver_access: ReceiverAccess,
        source: &Source,
    ) -> Option<InterfaceCallResult> {
        let Some(iface_def) = self.interface_registry.get(iface_name) else {
            self.report(TypeError::InterfaceNotAvailable {
                name: iface_name.into(),
                src: source.src(),
                span: source.span,
            });
            return None;
        };

        let Some(method_defs) = self.resolution.impls.get(&(struct_def, iface_def)) else {
            self.report(TypeError::InterfaceMethodMissing {
                interface: iface_name.into(),
                method: method_name.into(),
                src: source.src(),
                span: source.span,
            });
            return None;
        };

        let method_def_id = *method_defs
            .iter()
            .find(|&&def_id| self.def_name(def_id).as_deref() == Some(method_name))?;

        let sig = &self.fn_sigs[&method_def_id];
        let self_mode = sig.self_mode;
        let has_self = self_mode.is_some();

        match (self_mode, receiver_access) {
            (
                Some(SelfMode::Value) | Some(SelfMode::ValueConst),
                ReceiverAccess::RefMut | ReceiverAccess::RefConst,
            ) => {
                self.errors.push(TypeError::CannotMoveThroughPointer {
                    src: source.src(),
                    span: source.span,
                });
            }
            (Some(SelfMode::RefMut), ReceiverAccess::RefConst) => {
                self.errors.push(TypeError::AssignToConst {
                    src: source.src(),
                    span: source.span,
                });
            }
            (Some(SelfMode::RefMut) | Some(SelfMode::RefConst), ReceiverAccess::Value) => {}
            _ => {}
        }

        let sig_params = if has_self {
            sig.params[1..].to_vec()
        } else {
            sig.params.clone()
        };
        let sig_ret = sig.ret;

        if explicit_args.len() != sig_params.len() {
            self.report(TypeError::ArgCountMismatch {
                expected: sig_params.len(),
                found: explicit_args.len(),
                src: source.src(),
                span: source.span,
            });
        }

        let struct_generics = self
            .struct_generics
            .get(&struct_def)
            .cloned()
            .unwrap_or_default();
        let mut bindings: HashMap<DefId, TypeId> = struct_generics
            .iter()
            .copied()
            .zip(struct_generic_args.iter().copied())
            .collect();

        if let Some(impl_to_struct) = self
            .impl_generic_to_struct_generic
            .get(&(struct_def, iface_def))
        {
            for (&impl_g, &struct_g) in impl_to_struct {
                if let Some(&concrete) = bindings.get(&struct_g) {
                    bindings.insert(impl_g, concrete);
                }
            }
        }

        for (param_ty, &arg_ty) in sig_params.iter().zip(explicit_args.iter()) {
            let expected = self.substitute_generics(*param_ty, &bindings);

            if !try_coerce(&mut self.result.interner, arg_ty, expected).is_ok() {
                self.report(TypeError::Mismatch {
                    expected: self.display_type(expected).into(),
                    found: self.display_type(arg_ty).into(),
                    src: source.src(),
                    span: source.span,
                });
            }
        }

        Some(InterfaceCallResult {
            ret_ty: self.substitute_generics(sig_ret, &bindings),
            method_def: method_def_id,
        })
    }

    fn def_name(&self, def_id: DefId) -> Option<String> {
        self.resolution
            .defs
            .get(&def_id)
            .map(|info| self.interner.borrow().resolve(&info.name).to_string())
    }

    fn check_method_signature_matches(
        &mut self,
        iface_def: DefId,
        iface_method_def: DefId,
        impl_method_def: DefId,
        self_struct_ty: TypeId,
        imp_generics: &[DefId],
        source: &Source,
    ) {
        let iface_generics = self
            .interface_generics
            .get(&iface_def)
            .cloned()
            .unwrap_or_default();

        let mut generic_subst: HashMap<DefId, TypeId> = HashMap::new();
        for (iface_g, imp_g) in iface_generics.iter().zip(imp_generics.iter()) {
            let imp_g_ty = self.result.interner.intern(Type::GenericParam(*imp_g));
            generic_subst.insert(*iface_g, imp_g_ty);
        }

        let Some(iface_sig) = self.fn_sigs.get(&iface_method_def) else {
            return;
        };
        let iface_params_raw = iface_sig.params.clone();
        let iface_ret_raw = iface_sig.ret;

        let iface_params: Vec<TypeId> = iface_params_raw
            .iter()
            .map(|&p| {
                let p = self.substitute_self(p, self_struct_ty);
                self.substitute_generics(p, &generic_subst)
            })
            .collect();

        let iface_ret = {
            let r = self.substitute_self(iface_ret_raw, self_struct_ty);
            self.substitute_generics(r, &generic_subst)
        };

        let Some(impl_sig) = self.fn_sigs.get(&impl_method_def) else {
            return;
        };
        let impl_params = impl_sig.params.clone();
        let impl_ret = impl_sig.ret;

        let params_match = iface_params.len() == impl_params.len()
            && iface_params
                .iter()
                .zip(impl_params.iter())
                .all(|(a, b)| *a == *b);
        let ret_matches = iface_ret == impl_ret;

        if !params_match || !ret_matches {
            let method_name = self.def_name(iface_method_def).unwrap_or_default();
            let iface_name = self.def_name(iface_def).unwrap_or_default();
            let expected_signature = self.format_signature(&method_name, &iface_params, iface_ret);

            self.report(TypeError::InterfaceMethodSignatureMismatch {
                interface: iface_name.into(),
                method: method_name.into(),
                signature: expected_signature.into(),
                src: source.src(),
                span: source.span,
            });
        }
    }

    fn check_implement_matches_interface(
        &mut self,
        imp: &zeen_hir::HirImplement,
        iface_def: DefId,
        object_def: DefId,
        imp_generics: &[DefId],
        source: &Source,
    ) {
        let Some(interface_methods) = self.interface_methods.get(&iface_def).cloned() else {
            return;
        };

        let struct_generics = self
            .struct_generics
            .get(&object_def)
            .cloned()
            .unwrap_or_default();
        let self_generic_args: Vec<TypeId> = struct_generics
            .iter()
            .map(|&g| self.result.interner.intern(Type::GenericParam(g)))
            .collect();
        let self_struct_ty = self.result.interner.intern(Type::Struct {
            def_id: object_def,
            generic_args: self_generic_args,
        });

        let mut impl_method_names: HashMap<String, DefId> = HashMap::new();
        for method in &imp.methods {
            if let Some(name) = self.def_name(method.def_id) {
                impl_method_names.insert(name, method.def_id);
            }
        }

        let mut matched_names: HashSet<String> = HashSet::new();

        for &iface_method_def in &interface_methods {
            let Some(method_name) = self.def_name(iface_method_def) else {
                continue;
            };
            matched_names.insert(method_name.clone());

            match impl_method_names.get(&method_name) {
                None => {
                    self.report(TypeError::InterfaceMethodMissing {
                        interface: self.def_name(iface_def).unwrap_or_default().into(),
                        method: method_name.into(),
                        src: source.src(),
                        span: source.span,
                    });
                }
                Some(&impl_method_def) => {
                    self.check_method_signature_matches(
                        iface_def,
                        iface_method_def,
                        impl_method_def,
                        self_struct_ty,
                        imp_generics,
                        source,
                    );
                }
            }
        }
    }

    fn check_binary_op(
        &mut self,
        op: BinaryOp,
        lhs: TypeId,
        rhs: TypeId,
        expr_id: HirId,
        source: Source,
    ) -> TypeId {
        if lhs == self.result.interner.error() || rhs == self.result.interner.error() {
            return self.result.interner.error();
        }

        use BinaryOp::*;

        if matches!(op, Lt | Gt | Le | Ge) {
            return self.check_ordering_op(lhs, rhs, &source);
        }

        if matches!(op, LogicalAnd | LogicalOr) {
            return self.check_logical_op(lhs, rhs, &source);
        }

        match self.result.interner.get(lhs).clone() {
            Type::Struct {
                def_id,
                generic_args,
            } => {
                let Some((iface_name, method_name)) = binary_op_interface(op) else {
                    self.report(TypeError::BinaryNotSupported {
                        op,
                        lhs_type: self.display_type(lhs).into(),
                        rhs_type: self.display_type(rhs).into(),
                        src: source.src(),
                        span: source.span,
                    });
                    return self.result.interner.error();
                };

                match self.call_interface_method(
                    def_id,
                    &generic_args,
                    iface_name,
                    method_name,
                    &[rhs],
                    ReceiverAccess::Value,
                    &source,
                ) {
                    Some(r) => {
                        self.result.operator_resolutions.insert(
                            expr_id,
                            OperatorResolution {
                                method_def: r.method_def,
                                generic_args: generic_args.to_vec(),
                            },
                        );

                        r.ret_ty
                    }
                    None => self.result.interner.error(),
                }
            }

            Type::GenericParam(g) => self.check_binary_op_on_generic(op, g, lhs, rhs, &source),

            _ => self.check_binary_op_builtin(op, lhs, rhs, &source),
        }
    }

    fn check_binary_op_builtin(
        &mut self,
        op: BinaryOp,
        lhs: TypeId,
        rhs: TypeId,
        source: &Source,
    ) -> TypeId {
        let unified = if lhs == rhs {
            Some(lhs)
        } else if let Type::Pointer { .. } = self.result.interner.get(lhs)
            && let Type::Builtin(BuiltinType::usize) = self.result.interner.get(rhs)
        {
            Some(lhs)
        } else if try_coerce(&mut self.result.interner, lhs, rhs).is_ok() {
            Some(rhs)
        } else if try_coerce(&mut self.result.interner, rhs, lhs).is_ok() {
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
                    operand_ty
                } else {
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

            Eq | Ne => self.result.interner.builtin(BuiltinType::bool),

            _ => unreachable!("others must be handled before this fn"),
        }
    }

    fn check_ordering_op(&mut self, lhs: TypeId, rhs: TypeId, source: &Source) -> TypeId {
        let comparable = lhs == rhs
            || try_coerce(&mut self.result.interner, lhs, rhs).is_ok()
            || try_coerce(&mut self.result.interner, rhs, lhs).is_ok();

        if comparable && (self.is_numeric_or_literal(lhs) || self.is_numeric_or_literal(rhs)) {
            self.result.interner.builtin(BuiltinType::bool)
        } else {
            self.report(TypeError::BinaryNotSupported {
                op: BinaryOp::Lt,
                lhs_type: self.display_type(lhs).into(),
                rhs_type: self.display_type(rhs).into(),
                src: source.src(),
                span: source.span,
            });
            self.result.interner.error()
        }
    }

    fn check_logical_op(&mut self, lhs: TypeId, rhs: TypeId, source: &Source) -> TypeId {
        let bool_ty = self.result.interner.builtin(BuiltinType::bool);
        if lhs == bool_ty && rhs == bool_ty {
            bool_ty
        } else {
            self.report(TypeError::BinaryNotSupported {
                op: BinaryOp::Lt,
                lhs_type: self.display_type(lhs).into(),
                rhs_type: self.display_type(rhs).into(),
                src: source.src(),
                span: source.span,
            });
            self.result.interner.error()
        }
    }

    fn check_binary_op_on_generic(
        &mut self,
        op: BinaryOp,
        g: DefId,
        lhs: TypeId,
        rhs: TypeId,
        source: &Source,
    ) -> TypeId {
        let Some((iface_name, _method_name)) = binary_op_interface(op) else {
            self.report(TypeError::BinaryNotSupported {
                op: BinaryOp::Lt,
                lhs_type: self.display_type(lhs).into(),
                rhs_type: self.display_type(rhs).into(),
                src: source.src(),
                span: source.span,
            });
            return self.result.interner.error();
        };

        let Some(iface_def) = self.interface_registry.get(iface_name) else {
            self.report(TypeError::InterfaceNotAvailable {
                name: iface_name.into(),
                src: source.src(),
                span: source.span,
            });
            return self.result.interner.error();
        };

        let bounds = self.ctx.generic_bounds(g);
        if !bounds.contains(&iface_def) {
            self.report(TypeError::GenericMissingBound {
                generic: self.def_name(g).unwrap_or_default().into(),
                bound: iface_name.into(),
                src: source.src(),
                span: source.span,
            });
            return self.result.interner.error();
        }

        if rhs != lhs && !try_coerce(&mut self.result.interner, rhs, lhs).is_ok() {
            self.report(TypeError::Mismatch {
                expected: self.display_type(lhs).into(),
                found: self.display_type(rhs).into(),
                src: source.src(),
                span: source.span,
            });
        }

        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) {
            self.result.interner.builtin(BuiltinType::bool)
        } else {
            lhs
        }
    }

    fn check_unary_op(
        &mut self,
        op: UnaryOp,
        operand: TypeId,
        expr_id: HirId,
        source: Source,
    ) -> TypeId {
        if matches!(self.result.interner.get(operand), Type::Error) {
            return self.result.interner.error();
        }

        if let UnaryOp::AddrOf = op {
            return self.result.interner.intern(Type::Pointer {
                inner: operand,
                is_const: false,
            });
        }

        match self.result.interner.get(operand).clone() {
            Type::Struct {
                def_id,
                generic_args,
            } => {
                let Some((iface_name, method_name)) = unary_op_interface(op) else {
                    self.report(TypeError::UnaryNotSupported {
                        op,
                        child_type: self.display_type(operand).into(),
                        src: source.src(),
                        span: source.span,
                    });
                    return self.result.interner.error();
                };

                if matches!(op, UnaryOp::Deref) {
                    return self.check_deref_on_struct(def_id, &generic_args, expr_id, &source);
                }

                match self.call_interface_method(
                    def_id,
                    &generic_args,
                    iface_name,
                    method_name,
                    &[],
                    ReceiverAccess::Value,
                    &source,
                ) {
                    Some(r) => {
                        self.result.operator_resolutions.insert(
                            expr_id,
                            OperatorResolution {
                                method_def: r.method_def,
                                generic_args: generic_args.clone(),
                            },
                        );

                        r.ret_ty
                    }
                    None => self.result.interner.error(),
                }
            }

            Type::GenericParam(g) => {
                let Some((iface_name, method_name)) = unary_op_interface(op) else {
                    self.report(TypeError::UnaryNotSupported {
                        op,
                        child_type: self.display_type(operand).into(),
                        src: source.src(),
                        span: source.span,
                    });
                    return self.result.interner.error();
                };

                let (iface_name, _) =
                    if matches!(op, UnaryOp::Deref) && self.expect_assign_interface {
                        ("DerefAssign", "deref_assign")
                    } else {
                        (iface_name, method_name)
                    };

                let Some(iface_def) = self.interface_registry.get(iface_name) else {
                    self.report(TypeError::InterfaceNotAvailable {
                        name: iface_name.into(),
                        src: source.src(),
                        span: source.span,
                    });
                    return self.result.interner.error();
                };

                let bounds = self.ctx.generic_bounds(g);
                if !bounds.contains(&iface_def) {
                    self.report(TypeError::GenericMissingBound {
                        generic: self.def_name(g).unwrap_or_default().into(),
                        bound: iface_name.into(),
                        src: source.src(),
                        span: source.span,
                    });
                    return self.result.interner.error();
                }

                operand
            }

            Type::Pointer { inner, .. } if matches!(op, UnaryOp::Deref) => inner,

            Type::Builtin(b) => {
                let Some((iface_name, _)) = unary_op_interface(op) else {
                    self.report(TypeError::UnaryNotSupported {
                        op,
                        child_type: self.display_type(operand).into(),
                        src: source.src(),
                        span: source.span,
                    });
                    return self.result.interner.error();
                };
                if Self::builtin_interface_names(b).contains(&iface_name) {
                    match op {
                        UnaryOp::Not => self.result.interner.builtin(BuiltinType::bool),
                        _ => operand,
                    }
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

            Type::IntLiteral | Type::FloatLiteral => {
                if matches!(op, UnaryOp::Neg | UnaryOp::BitNot) {
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

    fn check_deref_on_struct(
        &mut self,
        def_id: DefId,
        generic_args: &[TypeId],
        expr_id: HirId,
        source: &Source,
    ) -> TypeId {
        let (iface_name, method_name) = if self.expect_assign_interface {
            ("DerefPtr", "deref_ptr")
        } else {
            ("Deref", "deref")
        };

        let result = self.call_interface_method(
            def_id,
            generic_args,
            iface_name,
            method_name,
            &[],
            ReceiverAccess::Value,
            source,
        );

        match result {
            Some(res) => {
                self.result.operator_resolutions.insert(
                    expr_id,
                    OperatorResolution {
                        method_def: res.method_def,
                        generic_args: generic_args.to_vec(),
                    },
                );

                if self.expect_assign_interface {
                    match self.result.interner.get(res.ret_ty).clone() {
                        Type::Pointer { inner, .. } => inner,
                        _ => res.ret_ty,
                    }
                } else {
                    res.ret_ty
                }
            }
            None => self.result.interner.error(),
        }
    }

    fn check_slice_access_on_struct(
        &mut self,
        def_id: DefId,
        generic_args: &[TypeId],
        index_ty: TypeId,
        expr_id: HirId,
        source: &Source,
    ) -> TypeId {
        let (iface_name, method_name) = if self.expect_assign_interface {
            ("SlicePtr", "slice_ptr")
        } else {
            ("Slice", "slice")
        };

        let result = self.call_interface_method(
            def_id,
            generic_args,
            iface_name,
            method_name,
            &[index_ty],
            ReceiverAccess::Value,
            source,
        );

        match result {
            Some(r) => {
                self.result.operator_resolutions.insert(
                    expr_id,
                    OperatorResolution {
                        method_def: r.method_def,
                        generic_args: generic_args.to_vec(),
                    },
                );

                if self.expect_assign_interface {
                    match self.result.interner.get(r.ret_ty).clone() {
                        Type::Pointer { inner, .. } => inner,
                        _ => r.ret_ty,
                    }
                } else {
                    r.ret_ty
                }
            }
            None => self.result.interner.error(),
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
