use std::{cell::RefCell, collections::HashMap, rc::Rc};

use lasso::Spur;
use zeen_ast::types::BuiltinType;
use zeen_hir::HirTypeKind;
use zeen_resolve::{DefId, DefInfo};

use crate::{DEFAULT_FLOAT_LITERAL, DEFAULT_INT_LITERAL};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(pub u32);

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

    Array {
        element: TypeId,
        len: Option<u64>,
    },

    Slice {
        element: TypeId,
    },

    Fn {
        params: Vec<TypeId>,
        ret: TypeId,
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

            Type::Array { element, len } => format!(
                "[{}]{}",
                len.map(|val| val.to_string()).unwrap_or_default(),
                type_interner.display_type(*element, interner, resolution_result)
            ),

            Type::Slice { element } => format!(
                "[]{}",
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

        HirTypeKind::Pointer(inner) => match &inner.kind {
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
