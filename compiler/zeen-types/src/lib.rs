use std::{cell::RefCell, collections::HashMap, rc::Rc};

use lasso::Spur;

use zeen_ast::types::BuiltinType;
use zeen_hir::HirTypeKind;
use zeen_resolve::DefId;

pub const DEFAULT_INT_LITERAL: BuiltinType = BuiltinType::i32;
pub const DEFAULT_FLOAT_LITERAL: BuiltinType = BuiltinType::f64;

/// Synthetic `DefId`s for the builtin slice's `{ ptr, len }` view and for a
/// fixed array's compile-time `.len`. They never appear in user declarations
/// and share no numbering with real `DefId`s (which stay small).
pub const SLICE_STRUCT_DEF: DefId = DefId(u32::MAX - 3);
pub const SLICE_PTR_FIELD: DefId = DefId(u32::MAX - 2);
pub const SLICE_LEN_FIELD: DefId = DefId(u32::MAX - 1);
pub const ARRAY_LEN_FIELD: DefId = DefId(u32::MAX - 4);

/// Synthetic `DefId`s for the fat closure-value struct `{ function, env }`
/// (type `Type::FatFn`). The struct def and its two fields are canonical — they
/// are shared by every fat value, since the layout of a fat pointer is always
/// two pointer-sized slots regardless of the captured environment's shape.
pub const CLOSURE_FAT_DEF: DefId = DefId(u32::MAX - 5);
pub const CLOSURE_FAT_FN_FIELD: DefId = DefId(u32::MAX - 6);
pub const CLOSURE_FAT_ENV_FIELD: DefId = DefId(u32::MAX - 7);

/// Synthetic `DefId`s for closure value structs. Each closure function gets a
/// private block of ids (struct def first, then one per field), far away from
/// real defs (small), slice/array sentinels (top of the range) and each other.
const CLOSURE_SYNTH_BASE: u32 = 1 << 30;
const CLOSURE_SYNTH_STRIDE: u32 = 1024;

/// The value-struct `DefId` of the closure defined by `closure_fn`.
pub fn closure_struct_def(closure_fn: DefId) -> DefId {
    DefId(CLOSURE_SYNTH_BASE + closure_fn.0 * CLOSURE_SYNTH_STRIDE)
}

/// The `i`-th field (0 = `$fn_ptr`, then captures) of the closure value struct.
pub fn closure_field_def(closure_fn: DefId, index: usize) -> DefId {
    DefId(CLOSURE_SYNTH_BASE + closure_fn.0 * CLOSURE_SYNTH_STRIDE + 1 + index as u32)
}

/// Whether `def_id` belongs to a synthetic closure value struct.
pub fn is_closure_struct_def(def_id: DefId) -> bool {
    def_id.0 >= CLOSURE_SYNTH_BASE
        && def_id.0 < CLOSURE_SYNTH_BASE + (u32::MAX - CLOSURE_SYNTH_BASE) / 2
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(pub u32);

/// What a fat fn value is made of.
///
/// Storage is always concrete: the captures live in an inline struct (the
/// value *is* the environment) and the called function is known statically.
/// `Bound` is the erased annotation form `Fn(T) R` — a coercion target used
/// for checks, never a storage type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FatFnBody {
    /// The annotation form `Fn(T) R` / `FnOnce(T) R`.
    Bound,
    /// A closure (or static `fn`) value: captures in an inline env struct,
    /// called by dispatching directly to `target` with `&env` as the first
    /// argument (env-first ABI).
    Closure { env: TypeId, target: DefId },
    /// A basic fn pointer value stored inline in a one-field struct; called
    /// indirectly through it with the plain (no-env) ABI.
    Pointer { pointee: TypeId },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    Builtin(BuiltinType),

    IntLiteral,   // not typed int literal, defaults to `i32`
    FloatLiteral, // same, defaults to `f64`

    Struct {
        def_id: DefId,
        generic_args: Vec<TypeId>,
    },

    Interface {
        def_id: DefId,
    },

    Enum {
        def_id: DefId,
    },

    Pointer {
        inner: TypeId,
        is_const: bool,
    },

    ManyPointer {
        inner: TypeId,
        is_const: bool,
    },

    Array {
        element: TypeId,
        len: Option<u64>,
    },

    Slice {
        element: TypeId,
        is_const: bool,
    },

    Fn {
        params: Vec<TypeId>,
        ret: TypeId,
    },

    /// Fat closure value. `once` marks `FnOnce` — callable at most once
    /// because it owns a non-Copy capture; `Fn` values (all-Copy captures or
    /// none) are `Copy`. The `body` says what the value is made of: storage
    /// is always concrete (inline env struct + static target, or an inline
    /// fn pointer), while `Bound` is the erased annotation form used only as
    /// a coercion target.
    FatFn {
        params: Vec<TypeId>,
        ret: TypeId,
        once: bool,
        body: FatFnBody,
    },

    GenericParam(DefId),
    InterfaceSelfPlaceholder(DefId),

    Void,
    Never,
    Error,
}

impl Type {
    pub fn to_display(
        &self,
        interner: Rc<RefCell<lasso::Rodeo>>,
        type_interner: &TypeInterner,
        resolution_result: &zeen_resolve::ResolutionResult,
    ) -> String {
        match self {
            Type::Builtin(b) => b.to_string(),
            Type::IntLiteral => DEFAULT_INT_LITERAL.to_string(),
            Type::FloatLiteral => DEFAULT_FLOAT_LITERAL.to_string(),

            Type::Struct {
                def_id,
                generic_args,
            } if is_closure_struct_def(*def_id) && !generic_args.is_empty() => {
                // Closure value structs carry their callable signature in
                // `generic_args[0]` and captured types after it, so they can
                // be displayed as a readable signature.
                let signature = type_interner.get(generic_args[0]).to_display(
                    Rc::clone(&interner),
                    type_interner,
                    resolution_result,
                );

                if generic_args.len() == 1 {
                    signature
                } else {
                    let env: Vec<String> = generic_args[1..]
                        .iter()
                        .map(|&ty| {
                            type_interner.get(ty).to_display(
                                Rc::clone(&interner),
                                type_interner,
                                resolution_result,
                            )
                        })
                        .collect();

                    format!("{} [env: {}]", signature, env.join(", "))
                }
            }

            Type::Struct {
                def_id,
                generic_args,
            } => {
                let name = resolution_result
                    .defs
                    .get(def_id)
                    .map(|info| interner.borrow().resolve(&info.name).to_string())
                    .unwrap_or("undefined".to_string());

                if generic_args.is_empty() {
                    name
                } else {
                    let args: Vec<String> = generic_args
                        .iter()
                        .map(|&a| {
                            type_interner.get(a).to_display(
                                Rc::clone(&interner),
                                type_interner,
                                resolution_result,
                            )
                        })
                        .collect();

                    format!("{}[{}]", name, args.join(", "))
                }
            }

            Type::Interface { def_id } | Type::Enum { def_id } | Type::GenericParam(def_id) => {
                resolution_result
                    .defs
                    .get(def_id)
                    .map(|info| interner.borrow().resolve(&info.name).to_string())
                    .unwrap_or("undefined".to_string())
            }

            Type::Pointer { inner, is_const } => format!(
                "*{}{}",
                if *is_const { "const " } else { "" },
                type_interner.display_type(*inner, interner, resolution_result)
            ),

            Type::ManyPointer { inner, is_const } => format!(
                "[*]{}{}",
                if *is_const { "const " } else { "" },
                type_interner.display_type(*inner, interner, resolution_result)
            ),

            Type::Array { element, len } => format!(
                "[{}]{}",
                len.map(|val| val.to_string()).unwrap_or_default(),
                type_interner.display_type(*element, interner, resolution_result)
            ),

            Type::Slice { element, is_const } => format!(
                "[]{}{}",
                if *is_const { "const " } else { "" },
                type_interner.display_type(*element, interner, resolution_result)
            ),

            Type::Fn { params, ret } => {
                let string_params = params
                    .iter()
                    .map(|param| {
                        type_interner.display_type(*param, Rc::clone(&interner), resolution_result)
                    })
                    .collect::<Vec<String>>();

                let string_ret = type_interner.display_type(*ret, interner, resolution_result);

                format!("fn({}) {}", string_params.join(", "), string_ret)
            }

            Type::FatFn {
                params, ret, once, ..
            } => {
                let string_params = params
                    .iter()
                    .map(|param| {
                        type_interner.display_type(*param, Rc::clone(&interner), resolution_result)
                    })
                    .collect::<Vec<String>>();

                let string_ret = type_interner.display_type(*ret, interner, resolution_result);

                let keyword = if *once { "FnOnce" } else { "Fn" };
                format!("{}({}) {}", keyword, string_params.join(", "), string_ret)
            }

            Type::InterfaceSelfPlaceholder(_) => "Self".into(),

            Type::Void => "void".into(),
            Type::Never => "never".into(),
            Type::Error => "error".into(),
        }
    }
}

#[derive(Debug, Default)]
pub struct TypeInterner {
    types: Vec<Type>,
    lookup: HashMap<Type, TypeId>,
}

impl TypeInterner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn intern(&mut self, ty: Type) -> TypeId {
        if let Some(id) = self.lookup.get(&ty) {
            return *id;
        }

        let id = TypeId(self.types.len() as u32);
        self.types.push(ty.clone());
        self.lookup.insert(ty, id);
        id
    }

    pub fn get(&self, id: TypeId) -> &Type {
        &self.types[id.0 as usize]
    }

    pub fn display_type(
        &self,
        id: TypeId,
        interner: Rc<RefCell<lasso::Rodeo>>,
        resolution_result: &zeen_resolve::ResolutionResult,
    ) -> String {
        let ty = self.get(id).clone();
        ty.to_display(interner, self, resolution_result)
    }

    pub fn builtin(&mut self, b: BuiltinType) -> TypeId {
        if b == BuiltinType::void {
            return self.void();
        }

        self.intern(Type::Builtin(b))
    }

    pub fn void(&mut self) -> TypeId {
        self.intern(Type::Void)
    }

    pub fn error(&mut self) -> TypeId {
        self.intern(Type::Error)
    }

    pub fn int_literal(&mut self) -> TypeId {
        self.intern(Type::IntLiteral)
    }

    pub fn float_literal(&mut self) -> TypeId {
        self.intern(Type::FloatLiteral)
    }

    pub fn never(&mut self) -> TypeId {
        self.intern(Type::Never)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    pub is_copy: bool,
    pub has_explicit_drop: bool,
}

impl Capabilities {
    pub const COPY: Capabilities = Capabilities {
        is_copy: true,
        has_explicit_drop: false,
    };

    pub const MOVE_ONLY: Capabilities = Capabilities {
        is_copy: false,
        has_explicit_drop: false,
    };
}

#[derive(Debug, Clone)]
pub struct StructTypeInfo {
    pub def_id: DefId,
    /// (name, field DefId, field TypeId)
    pub fields: Vec<StructFieldInfo>,
    pub capabalities: Capabilities,
}

#[derive(Debug, Clone)]
pub struct StructFieldInfo {
    pub name: Spur,
    pub field_def: DefId,
    pub field_ty: TypeId,
    pub struct_def: DefId,
    pub is_pub: bool,
}

/// Enum for `self` reciever representation:
/// - `fn method(self)` - Value (takes ownership)
/// - `fn method(const self)` - ValueConst (takes ownership, const binding)
/// - `fn method(*self)` - RefMut (no ownership transfer, mutable pointer)
/// - `fn method(*const self)` - RefMut (no ownership transfer, const pointer)
///
/// **Please note that** pointers of `self` are constant variables (not data, variables), that means
/// you cannot reassign self pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelfMode {
    Value,
    ValueConst,
    RefMut,
    RefConst,
}

#[allow(unused)]
impl SelfMode {
    pub fn takes_ownership(self) -> bool {
        matches!(self, SelfMode::Value | SelfMode::ValueConst)
    }

    pub fn is_const(self) -> bool {
        matches!(self, SelfMode::ValueConst | SelfMode::RefConst)
    }
}

/// Extracts `SelfMode` representation from TypeKind
pub fn self_mode_of(ty: &HirTypeKind) -> Option<SelfMode> {
    match ty {
        HirTypeKind::SelfType(_) | HirTypeKind::SelfAlias(_) => Some(SelfMode::Value),

        HirTypeKind::Const(inner) => match &inner.kind {
            HirTypeKind::SelfType(_) | HirTypeKind::SelfAlias(_) => Some(SelfMode::ValueConst),
            _ => None,
        },

        HirTypeKind::SinglePointer(inner) => match &inner.kind {
            HirTypeKind::SelfType(_) | HirTypeKind::SelfAlias(_) => Some(SelfMode::RefMut),
            HirTypeKind::Const(c)
                if matches!(c.kind, HirTypeKind::SelfType(_) | HirTypeKind::SelfAlias(_)) =>
            {
                Some(SelfMode::RefConst)
            }

            _ => None,
        },

        _ => None,
    }
}

/// Representation how the caller is accessing a struct instance when invoking interface method on it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(unused)]
pub enum ReceiverAccess {
    Value,
    RefMut,
    RefConst,
}

// Below is maps for interface operators.

pub fn binary_op_interface(
    op: zeen_ast::expressions::BinaryOp,
) -> Option<(&'static str, &'static str)> {
    use zeen_ast::expressions::BinaryOp::*;
    match op {
        Add => Some(("Add", "add")),
        Sub => Some(("Sub", "sub")),
        Mul => Some(("Mul", "mul")),
        Div => Some(("Div", "div")),
        Mod => Some(("Mod", "mod")),
        BitAnd => Some(("BitAnd", "bit_and")),
        BitOr => Some(("BitOr", "bit_or")),
        BitXor => Some(("BitXor", "bit_xor")),
        Shl => Some(("BitShl", "bit_shl")),
        Shr => Some(("BitShr", "bit_shr")),
        Eq | Ne => Some(("Eq", "eq")),
        Lt | Gt | Le | Ge | LogicalAnd | LogicalOr => None,
    }
}

pub fn unary_op_interface(
    op: zeen_ast::expressions::UnaryOp,
) -> Option<(&'static str, &'static str)> {
    use zeen_ast::expressions::UnaryOp::*;
    match op {
        Neg => Some(("Neg", "neg")),
        Not => Some(("Not", "not")),
        BitNot => Some(("BitNot", "bit_not")),
        Deref => Some(("Deref", "deref")),
        AddrOf => None,
    }
}

pub fn substitute_generics(
    interner: &mut TypeInterner,
    ty: TypeId,
    bindings: &HashMap<DefId, TypeId>,
) -> TypeId {
    match interner.get(ty).clone() {
        Type::GenericParam(g) => bindings.get(&g).copied().unwrap_or(ty),

        Type::Pointer { inner, is_const } => {
            let new_inner = substitute_generics(interner, inner, bindings);
            if new_inner == inner {
                ty
            } else {
                interner.intern(Type::Pointer {
                    inner: new_inner,
                    is_const,
                })
            }
        }

        Type::ManyPointer { inner, is_const } => {
            let new_inner = substitute_generics(interner, inner, bindings);
            if new_inner == inner {
                ty
            } else {
                interner.intern(Type::ManyPointer {
                    inner: new_inner,
                    is_const,
                })
            }
        }

        Type::Slice { element, is_const } => {
            let new_element = substitute_generics(interner, element, bindings);
            if new_element == element {
                ty
            } else {
                interner.intern(Type::Slice {
                    element: new_element,
                    is_const,
                })
            }
        }

        Type::Array { element, len } => {
            let new_elem = substitute_generics(interner, element, bindings);
            if new_elem == element {
                ty
            } else {
                interner.intern(Type::Array {
                    element: new_elem,
                    len,
                })
            }
        }

        Type::Struct {
            def_id,
            generic_args,
        } => {
            let new_args: Vec<TypeId> = generic_args
                .iter()
                .map(|a| substitute_generics(interner, *a, bindings))
                .collect();
            if new_args == generic_args {
                ty
            } else {
                interner.intern(Type::Struct {
                    def_id,
                    generic_args: new_args,
                })
            }
        }

        Type::Fn { params, ret } => {
            let new_params: Vec<TypeId> = params
                .iter()
                .map(|p| substitute_generics(interner, *p, bindings))
                .collect();
            let new_ret = substitute_generics(interner, ret, bindings);
            if new_params == params && new_ret == ret {
                ty
            } else {
                interner.intern(Type::Fn {
                    params: new_params,
                    ret: new_ret,
                })
            }
        }

        _ => ty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

    use lasso::Rodeo;
    use miette::NamedSource;
    use zeen_ast::Source;
    use zeen_ast::expressions::{BinaryOp, UnaryOp};
    use zeen_ast::types::BuiltinType;
    use zeen_hir::{HirId, types::HirTypeExpr};
    use zeen_resolve::{DefId, DefInfo, DefKind, ResolutionResult};

    fn src(span: miette::SourceSpan) -> Source {
        Source {
            span,
            src: NamedSource::new("test.zn", Arc::new(String::new())),
        }
    }

    fn type_expr(kind: HirTypeKind) -> Rc<HirTypeExpr> {
        Rc::new(HirTypeExpr {
            id: HirId(0),
            kind,
            source: src(0.into()),
        })
    }

    fn insert_def(
        resolution: &mut ResolutionResult,
        interner: &mut Rodeo,
        id: DefId,
        name: &str,
        kind: DefKind,
    ) {
        resolution.defs.insert(
            id,
            DefInfo {
                name: interner.get_or_intern(name),
                kind,
                span: src(0.into()),
                decl: None,
                is_pub: false,
            },
        );
    }

    #[test]
    fn intern_deduplicates_equal_types() {
        let mut interner = TypeInterner::new();

        let a = interner.intern(Type::Builtin(BuiltinType::i32));
        let b = interner.intern(Type::Builtin(BuiltinType::i32));

        assert_eq!(a, b);
        assert_eq!(interner.get(a), &Type::Builtin(BuiltinType::i32));
    }

    #[test]
    fn intern_distinct_types_get_distinct_ids() {
        let mut interner = TypeInterner::new();

        let i32 = interner.intern(Type::Builtin(BuiltinType::i32));
        let f64 = interner.intern(Type::Builtin(BuiltinType::f64));

        assert_ne!(i32, f64);
    }

    #[test]
    fn intern_records_pointer_and_structural_types() {
        let mut interner = TypeInterner::new();
        let inner = interner.intern(Type::Builtin(BuiltinType::char));

        let ptr = interner.intern(Type::Pointer {
            inner,
            is_const: false,
        });
        let ptr_const = interner.intern(Type::Pointer {
            inner,
            is_const: true,
        });

        assert_ne!(ptr, ptr_const);
        assert_eq!(
            interner.get(ptr),
            &Type::Pointer {
                inner,
                is_const: false
            }
        );
        assert_eq!(
            interner.get(ptr_const),
            &Type::Pointer {
                inner,
                is_const: true
            }
        );
    }

    #[test]
    fn builtin_void_forwards_to_void() {
        let mut interner = TypeInterner::new();

        let builtin_void = interner.builtin(BuiltinType::void);
        let plain_void = interner.void();

        assert_eq!(builtin_void, plain_void);
        assert_eq!(interner.get(builtin_void), &Type::Void);
    }

    #[test]
    fn literal_helpers_mark_types() {
        let mut interner = TypeInterner::new();

        assert_eq!(interner.int_literal(), interner.intern(Type::IntLiteral));
        assert_eq!(
            interner.float_literal(),
            interner.intern(Type::FloatLiteral)
        );
        assert_eq!(interner.never(), interner.intern(Type::Never));
        assert_eq!(interner.error(), interner.intern(Type::Error));
    }

    #[test]
    fn builtin_defaults_sane() {
        assert_eq!(DEFAULT_INT_LITERAL, BuiltinType::i32);
        assert_eq!(DEFAULT_FLOAT_LITERAL, BuiltinType::f64);
    }

    #[test]
    fn display_builtin_and_literals() {
        let mut interner = TypeInterner::new();
        let resolution = ResolutionResult::default();
        let rodeo = Rc::new(RefCell::new(Rodeo::default()));

        let i32 = interner.intern(Type::Builtin(BuiltinType::i32));
        let f64 = interner.intern(Type::Builtin(BuiltinType::f64));
        let int_lit = interner.int_literal();
        let float_lit = interner.float_literal();
        assert_eq!(
            interner.display_type(i32, Rc::clone(&rodeo), &resolution),
            "i32"
        );
        assert_eq!(
            interner.display_type(f64, Rc::clone(&rodeo), &resolution),
            "f64"
        );
        assert_eq!(
            interner.display_type(int_lit, Rc::clone(&rodeo), &resolution),
            "i32"
        );
        assert_eq!(
            interner.display_type(float_lit, Rc::clone(&rodeo), &resolution),
            "f64"
        );
    }

    #[test]
    fn display_pointers_arrays_and_slices() {
        let mut interner = TypeInterner::new();
        let resolution = ResolutionResult::default();
        let inner = interner.intern(Type::Builtin(BuiltinType::i32));

        let ptr = interner.intern(Type::Pointer {
            inner,
            is_const: false,
        });
        let ptr_const = interner.intern(Type::Pointer {
            inner,
            is_const: true,
        });
        let many = interner.intern(Type::ManyPointer {
            inner,
            is_const: false,
        });
        let many_const = interner.intern(Type::ManyPointer {
            inner,
            is_const: true,
        });
        let array = interner.intern(Type::Array {
            element: inner,
            len: Some(4),
        });
        let array_unknown = interner.intern(Type::Array {
            element: inner,
            len: None,
        });
        let slice = interner.intern(Type::Slice {
            element: inner,
            is_const: false,
        });
        let slice_const = interner.intern(Type::Slice {
            element: inner,
            is_const: true,
        });

        assert_eq!(
            interner.display_type(ptr, Rc::new(RefCell::new(Rodeo::default())), &resolution),
            "*i32"
        );
        assert_eq!(
            interner.display_type(
                ptr_const,
                Rc::new(RefCell::new(Rodeo::default())),
                &resolution
            ),
            "*const i32"
        );
        assert_eq!(
            interner.display_type(many, Rc::new(RefCell::new(Rodeo::default())), &resolution),
            "[*]i32"
        );
        assert_eq!(
            interner.display_type(
                many_const,
                Rc::new(RefCell::new(Rodeo::default())),
                &resolution
            ),
            "[*]const i32"
        );
        assert_eq!(
            interner.display_type(array, Rc::new(RefCell::new(Rodeo::default())), &resolution),
            "[4]i32"
        );
        assert_eq!(
            interner.display_type(
                array_unknown,
                Rc::new(RefCell::new(Rodeo::default())),
                &resolution
            ),
            "[]i32"
        );
        assert_eq!(
            interner.display_type(slice, Rc::new(RefCell::new(Rodeo::default())), &resolution),
            "[]i32"
        );
        assert_eq!(
            interner.display_type(
                slice_const,
                Rc::new(RefCell::new(Rodeo::default())),
                &resolution
            ),
            "[]const i32"
        );
    }

    #[test]
    fn display_struct_type_with_generic_args() {
        let mut interner = TypeInterner::new();
        let mut resolution = ResolutionResult::default();
        let mut rodeo = Rodeo::default();

        let struct_def = DefId(1);
        insert_def(
            &mut resolution,
            &mut rodeo,
            struct_def,
            "Foo",
            DefKind::Struct,
        );

        let i32 = interner.intern(Type::Builtin(BuiltinType::i32));
        let f64 = interner.intern(Type::Builtin(BuiltinType::f64));
        let ty = interner.intern(Type::Struct {
            def_id: struct_def,
            generic_args: vec![i32, f64],
        });

        let result = interner.display_type(ty, Rc::new(RefCell::new(rodeo)), &resolution);
        assert_eq!(result, "Foo[i32, f64]");
    }

    #[test]
    fn display_named_types_by_name() {
        let mut interner = TypeInterner::new();
        let mut resolution = ResolutionResult::default();
        let mut rodeo = Rodeo::default();

        let iface = DefId(0);
        let en = DefId(1);
        let generic = DefId(2);
        insert_def(
            &mut resolution,
            &mut rodeo,
            iface,
            "Movable",
            DefKind::Interface,
        );
        insert_def(&mut resolution, &mut rodeo, en, "Color", DefKind::Enum);
        insert_def(
            &mut resolution,
            &mut rodeo,
            generic,
            "T",
            DefKind::GenericParam,
        );

        let iface_ty = interner.intern(Type::Interface { def_id: iface });
        let enum_ty = interner.intern(Type::Enum { def_id: en });
        let generic_ty = interner.intern(Type::GenericParam(generic));

        assert_eq!(
            interner.display_type(iface_ty, Rc::new(RefCell::new(rodeo.clone())), &resolution),
            "Movable"
        );
        assert_eq!(
            interner.display_type(enum_ty, Rc::new(RefCell::new(rodeo.clone())), &resolution),
            "Color"
        );
        assert_eq!(
            interner.display_type(generic_ty, Rc::new(RefCell::new(rodeo)), &resolution),
            "T"
        );
    }

    #[test]
    fn function_interface_self_placeholder_display() {
        let mut interner = TypeInterner::new();
        let resolution = ResolutionResult::default();
        let rodeo = Rc::new(RefCell::new(Rodeo::default()));
        let i32 = interner.intern(Type::Builtin(BuiltinType::i32));
        let f64 = interner.intern(Type::Builtin(BuiltinType::f64));
        let void = interner.void();

        let fn_ty = interner.intern(Type::Fn {
            params: vec![i32, f64],
            ret: void,
        });
        assert_eq!(
            interner.display_type(fn_ty, Rc::clone(&rodeo), &resolution),
            "fn(i32, f64) void"
        );

        let placeholder = interner.intern(Type::InterfaceSelfPlaceholder(DefId(3)));
        assert_eq!(
            interner.display_type(placeholder, Rc::clone(&rodeo), &resolution),
            "Self"
        );

        assert_eq!(
            interner.display_type(void, Rc::clone(&rodeo), &resolution),
            "void"
        );
        let never = interner.never();
        let error = interner.error();
        assert_eq!(
            interner.display_type(never, Rc::clone(&rodeo), &resolution),
            "never"
        );
        assert_eq!(
            interner.display_type(error, Rc::clone(&rodeo), &resolution),
            "error"
        );
    }

    #[test]
    fn closure_struct_displays_as_signature_with_env() {
        use zeen_resolve::DefId;

        let mut interner = TypeInterner::new();
        let resolution = ResolutionResult::default();
        let rodeo = Rc::new(RefCell::new(Rodeo::default()));

        let i32 = interner.intern(Type::Builtin(BuiltinType::i32));
        let fn_ty = interner.intern(Type::Fn {
            params: vec![i32],
            ret: i32,
        });

        let closure_def = closure_struct_def(DefId(7));
        let closure_ty = interner.intern(Type::Struct {
            def_id: closure_def,
            generic_args: vec![fn_ty, i32],
        });

        assert_eq!(
            interner.display_type(closure_ty, Rc::clone(&rodeo), &resolution),
            "fn(i32) i32 [env: i32]"
        );
    }

    #[test]
    fn fat_fn_displays_as_signature() {
        use zeen_resolve::DefId;

        let mut interner = TypeInterner::new();
        let resolution = ResolutionResult::default();
        let rodeo = Rc::new(RefCell::new(Rodeo::default()));

        let foo_def = DefId(4);
        let foo_name = rodeo.borrow_mut().get_or_intern("Foo");

        let i32 = interner.intern(Type::Builtin(BuiltinType::i32));
        let void = interner.intern(Type::Builtin(BuiltinType::void));
        let foo = interner.intern(Type::Struct {
            def_id: foo_def,
            generic_args: Vec::new(),
        });

        let fat = interner.intern(Type::FatFn {
            params: vec![i32],
            ret: i32,
            once: false,
            body: FatFnBody::Bound,
        });
        let fat_once = interner.intern(Type::FatFn {
            params: vec![foo],
            ret: void,
            once: true,
            body: FatFnBody::Bound,
        });

        assert_eq!(
            interner.display_type(fat, Rc::clone(&rodeo), &resolution),
            "Fn(i32) i32"
        );
        assert_eq!(
            interner.display_type(fat_once, Rc::clone(&rodeo), &resolution),
            "FnOnce(undefined) void"
        );

        let mut defs = ResolutionResult::default().defs;
        defs.insert(
            foo_def,
            zeen_resolve::DefInfo {
                name: foo_name,
                kind: zeen_resolve::DefKind::Struct,
                span: (
                    miette::SourceSpan::from((0, 0)),
                    miette::NamedSource::new("test.zn", std::sync::Arc::new(String::new())),
                )
                    .into(),
                decl: None,
                is_pub: false,
            },
        );

        assert_eq!(
            interner.display_type(
                fat_once,
                Rc::clone(&rodeo),
                &zeen_resolve::ResolutionResult {
                    defs,
                    ..Default::default()
                }
            ),
            "FnOnce(Foo) void"
        );
    }

    #[test]
    fn display_unresolved_def_is_undefined() {
        let mut interner = TypeInterner::new();
        let resolution = ResolutionResult::default();

        let unknown = interner.intern(Type::Struct {
            def_id: DefId(99),
            generic_args: Vec::new(),
        });
        assert_eq!(
            interner.display_type(
                unknown,
                Rc::new(RefCell::new(Rodeo::default())),
                &resolution
            ),
            "undefined"
        );
    }

    #[test]
    fn substitute_generics_noop_without_bindings() {
        let mut interner = TypeInterner::new();
        let generic = DefId(1);
        let ty = interner.intern(Type::GenericParam(generic));
        let bindings = HashMap::new();

        assert_eq!(substitute_generics(&mut interner, ty, &bindings), ty);
    }

    #[test]
    fn substitute_generics_replaces_param() {
        let mut interner = TypeInterner::new();
        let generic = DefId(1);
        let ty = interner.intern(Type::GenericParam(generic));
        let replacement = interner.intern(Type::Builtin(BuiltinType::i32));
        let mut bindings = HashMap::new();
        bindings.insert(generic, replacement);

        assert_eq!(
            substitute_generics(&mut interner, ty, &bindings),
            replacement
        );
    }

    #[test]
    fn substitute_generics_preserves_identity_when_unchanged() {
        let mut interner = TypeInterner::new();
        let generic = DefId(1);
        let inner = interner.intern(Type::Builtin(BuiltinType::i32));
        let ptr = interner.intern(Type::Pointer {
            inner,
            is_const: false,
        });
        let mut bindings = HashMap::new();
        bindings.insert(generic, inner);

        assert_eq!(substitute_generics(&mut interner, ptr, &bindings), ptr);
    }

    #[test]
    fn substitute_generics_rewrites_nested_types() {
        let mut interner = TypeInterner::new();
        let generic = DefId(1);
        let inner = interner.intern(Type::GenericParam(generic));
        let ptr = interner.intern(Type::Pointer {
            inner,
            is_const: false,
        });
        let replacement = interner.intern(Type::Builtin(BuiltinType::u8));
        let mut bindings = HashMap::new();
        bindings.insert(generic, replacement);

        let rewritten = substitute_generics(&mut interner, ptr, &bindings);
        assert_eq!(
            interner.get(rewritten),
            &Type::Pointer {
                inner: replacement,
                is_const: false
            }
        );
        assert_ne!(rewritten, ptr);
    }

    #[test]
    fn substitute_generics_descends_into_struct_and_fn() {
        let mut interner = TypeInterner::new();
        let generic = DefId(1);
        let struct_def = DefId(2);
        let generic_arg = interner.intern(Type::GenericParam(generic));
        let struct_ty = interner.intern(Type::Struct {
            def_id: struct_def,
            generic_args: vec![generic_arg],
        });
        let void = interner.void();
        let fn_ty = interner.intern(Type::Fn {
            params: vec![generic_arg],
            ret: void,
        });
        let replacement = interner.intern(Type::Builtin(BuiltinType::i64));
        let mut bindings = HashMap::new();
        bindings.insert(generic, replacement);

        let new_struct = substitute_generics(&mut interner, struct_ty, &bindings);
        assert_eq!(
            interner.get(new_struct),
            &Type::Struct {
                def_id: struct_def,
                generic_args: vec![replacement]
            }
        );

        let new_fn = substitute_generics(&mut interner, fn_ty, &bindings);
        let expected_fn = Type::Fn {
            params: vec![replacement],
            ret: void,
        };
        assert_eq!(interner.get(new_fn), &expected_fn);
    }

    #[test]
    fn self_mode_of_plain_self() {
        let def_id = DefId(1);
        assert_eq!(
            self_mode_of(&HirTypeKind::SelfType(def_id)),
            Some(SelfMode::Value)
        );
        assert_eq!(
            self_mode_of(&HirTypeKind::SelfAlias(def_id)),
            Some(SelfMode::Value)
        );
    }

    #[test]
    fn self_mode_of_const_self() {
        let def_id = DefId(1);
        let const_self = HirTypeKind::Const(type_expr(HirTypeKind::SelfType(def_id)));
        let const_alias = HirTypeKind::Const(type_expr(HirTypeKind::SelfAlias(def_id)));
        assert_eq!(self_mode_of(&const_self), Some(SelfMode::ValueConst));
        assert_eq!(self_mode_of(&const_alias), Some(SelfMode::ValueConst));
    }

    #[test]
    fn self_mode_of_pointer_self() {
        let def_id = DefId(1);
        assert_eq!(
            self_mode_of(&HirTypeKind::SinglePointer(type_expr(
                HirTypeKind::SelfType(def_id)
            ))),
            Some(SelfMode::RefMut)
        );
        assert_eq!(
            self_mode_of(&HirTypeKind::SinglePointer(type_expr(
                HirTypeKind::SelfAlias(def_id)
            ))),
            Some(SelfMode::RefMut)
        );
    }

    #[test]
    fn self_mode_of_const_pointer_self() {
        let def_id = DefId(1);
        let const_self = HirTypeKind::Const(type_expr(HirTypeKind::SelfType(def_id)));
        let const_alias = HirTypeKind::Const(type_expr(HirTypeKind::SelfAlias(def_id)));
        assert_eq!(
            self_mode_of(&HirTypeKind::SinglePointer(type_expr(const_self))),
            Some(SelfMode::RefConst)
        );
        assert_eq!(
            self_mode_of(&HirTypeKind::SinglePointer(type_expr(const_alias))),
            Some(SelfMode::RefConst)
        );
    }

    #[test]
    fn self_mode_of_ignores_other_types() {
        assert_eq!(self_mode_of(&HirTypeKind::Builtin(BuiltinType::i32)), None);
        assert_eq!(self_mode_of(&HirTypeKind::Error), None);
        assert_eq!(
            self_mode_of(&HirTypeKind::Const(type_expr(HirTypeKind::Builtin(
                BuiltinType::i32
            )))),
            None
        );
        assert_eq!(
            self_mode_of(&HirTypeKind::SinglePointer(type_expr(
                HirTypeKind::Builtin(BuiltinType::i32)
            ))),
            None
        );
    }

    #[test]
    fn binary_op_interface_maps_overloadable() {
        use BinaryOp::*;
        assert_eq!(binary_op_interface(Add), Some(("Add", "add")));
        assert_eq!(binary_op_interface(Sub), Some(("Sub", "sub")));
        assert_eq!(binary_op_interface(Mul), Some(("Mul", "mul")));
        assert_eq!(binary_op_interface(Div), Some(("Div", "div")));
        assert_eq!(binary_op_interface(Mod), Some(("Mod", "mod")));
        assert_eq!(binary_op_interface(BitAnd), Some(("BitAnd", "bit_and")));
        assert_eq!(binary_op_interface(BitOr), Some(("BitOr", "bit_or")));
        assert_eq!(binary_op_interface(BitXor), Some(("BitXor", "bit_xor")));
        assert_eq!(binary_op_interface(Shl), Some(("BitShl", "bit_shl")));
        assert_eq!(binary_op_interface(Shr), Some(("BitShr", "bit_shr")));
        assert_eq!(binary_op_interface(Eq), Some(("Eq", "eq")));
        assert_eq!(binary_op_interface(Ne), Some(("Eq", "eq")));
    }

    #[test]
    fn binary_op_interface_skips_non_overloadable() {
        use BinaryOp::*;
        assert_eq!(binary_op_interface(Lt), None);
        assert_eq!(binary_op_interface(Gt), None);
        assert_eq!(binary_op_interface(Le), None);
        assert_eq!(binary_op_interface(Ge), None);
        assert_eq!(binary_op_interface(LogicalAnd), None);
        assert_eq!(binary_op_interface(LogicalOr), None);
    }

    #[test]
    fn unary_op_interface_maps_overloadable() {
        use UnaryOp::*;
        assert_eq!(unary_op_interface(Neg), Some(("Neg", "neg")));
        assert_eq!(unary_op_interface(Not), Some(("Not", "not")));
        assert_eq!(unary_op_interface(BitNot), Some(("BitNot", "bit_not")));
        assert_eq!(unary_op_interface(Deref), Some(("Deref", "deref")));
        assert_eq!(unary_op_interface(AddrOf), None);
    }

    #[test]
    fn capabilities_constants() {
        let copy = Capabilities::COPY;
        let move_only = Capabilities::MOVE_ONLY;

        assert!(copy.is_copy);
        assert!(!copy.has_explicit_drop);
        assert!(!move_only.is_copy);
        assert!(!move_only.has_explicit_drop);
    }

    #[test]
    fn self_mode_ownership_and_constness() {
        assert!(SelfMode::Value.takes_ownership());
        assert!(SelfMode::ValueConst.takes_ownership());
        assert!(!SelfMode::RefMut.takes_ownership());
        assert!(!SelfMode::RefConst.takes_ownership());

        assert!(!SelfMode::Value.is_const());
        assert!(SelfMode::ValueConst.is_const());
        assert!(!SelfMode::RefMut.is_const());
        assert!(SelfMode::RefConst.is_const());
    }
}
