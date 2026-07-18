use crate::types::{Type, TypeId, TypeInterner};
use zeen_ast::types::BuiltinType;

pub fn builtin_is_integer(b: BuiltinType) -> bool {
    matches!(
        b,
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
    )
}

pub fn builtin_is_float(b: BuiltinType) -> bool {
    matches!(b, BuiltinType::f32 | BuiltinType::f64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoerceResult {
    /// Identical types, no conversion needed
    Identity,
    /// `from` was literal type, now pinned to `to`
    PinLiteral,
    /// Type was non const, add const modificator
    AddConst,
    /// Type was const, remove const modificator
    RemoveConst,
    // Fixed array to slice: [N]T -> []T
    ArrayToSlice,
    // Fixed array to many ptr: [N]T -> [*]T
    ArrayToManyPointer,
    // `never` -> any (for example: @panic(format, ...) macro)
    NeverCoercion,
    /// One side was `Error`, avoiding extra diagnostics
    ErrorRecovery,
    // Error coercion
    Fail,
}

impl CoerceResult {
    pub fn is_ok(self) -> bool {
        !matches!(self, CoerceResult::Fail)
    }

    pub fn is_err(self) -> bool {
        matches!(self, CoerceResult::Fail)
    }
}

pub fn try_coerce(interner: &TypeInterner, from: TypeId, to: TypeId) -> CoerceResult {
    if from == to {
        return CoerceResult::Identity;
    }

    let from_ty = interner.get(from);
    let to_ty = interner.get(to);

    if matches!(from_ty, Type::Error) || matches!(to_ty, Type::Error) {
        return CoerceResult::ErrorRecovery;
    }

    if matches!(from_ty, Type::Never) {
        return CoerceResult::NeverCoercion;
    }

    match (from_ty, to_ty) {
        (Type::IntLiteral, Type::Builtin(b)) if builtin_is_integer(*b) => CoerceResult::PinLiteral,
        (Type::FloatLiteral, Type::Builtin(b)) if builtin_is_float(*b) => CoerceResult::PinLiteral,

        (
            Type::Pointer {
                inner: from_inner,
                is_const: false,
            },
            Type::Pointer {
                inner: to_inner,
                is_const: true,
            },
        ) if from_inner == to_inner => CoerceResult::AddConst,

        (
            Type::Pointer {
                inner: from_inner,
                is_const: true,
            },
            Type::Pointer {
                inner: to_inner,
                is_const: false,
            },
        ) if from_inner == to_inner => CoerceResult::RemoveConst,

        (
            Type::Array {
                element: from_elem, ..
            },
            Type::Slice {
                element: to_elem, ..
            },
        ) if from_elem == to_elem => CoerceResult::ArrayToSlice,

        (
            Type::ManyPointer {
                inner: fi,
                is_const: false,
            },
            Type::ManyPointer {
                inner: ti,
                is_const: true,
            },
        ) if fi == ti => CoerceResult::AddConst,

        (
            Type::Slice {
                element: fe,
                is_const: false,
            },
            Type::Slice {
                element: te,
                is_const: true,
            },
        ) if fe == te => CoerceResult::AddConst,

        (
            Type::Slice {
                element: fe,
                is_const: true,
            },
            Type::Slice {
                element: te,
                is_const: false,
            },
        ) if fe == te => CoerceResult::RemoveConst,

        (
            Type::Array { element: fe, .. },
            Type::Slice {
                element: te,
                is_const: _,
            },
        ) if fe == te => CoerceResult::ArrayToSlice,

        (Type::Array { element: fe, .. }, Type::ManyPointer { inner: te, .. }) if fe == te => {
            CoerceResult::ArrayToManyPointer
        }

        _ => CoerceResult::Fail,
    }
}

pub fn is_coercible(interner: &TypeInterner, from: TypeId, to: TypeId) -> bool {
    try_coerce(interner, from, to).is_ok()
}
