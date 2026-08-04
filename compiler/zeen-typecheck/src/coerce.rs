use zeen_ast::types::BuiltinType;
use zeen_types::{Type, TypeId, TypeInterner};

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
    // Void pointer is a universal pointer
    VoidPtrCoercion,
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
}

pub fn try_coerce(interner: &mut TypeInterner, from: TypeId, to: TypeId) -> CoerceResult {
    if from == to {
        return CoerceResult::Identity;
    }

    let from_ty = interner.get(from).clone();
    let to_ty = interner.get(to).clone();

    if matches!(from_ty, Type::Error) || matches!(to_ty, Type::Error) {
        return CoerceResult::ErrorRecovery;
    }

    if matches!(from_ty, Type::Never) {
        return CoerceResult::NeverCoercion;
    }

    match (from_ty, to_ty) {
        (Type::IntLiteral, Type::Builtin(b)) if builtin_is_integer(b) => CoerceResult::PinLiteral,
        (Type::FloatLiteral, Type::Builtin(b)) if builtin_is_float(b) => CoerceResult::PinLiteral,

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
            Type::Pointer {
                inner: from_inner, ..
            },
            Type::Pointer {
                inner: to_inner, ..
            },
        ) if from_inner == interner.void() || to_inner == interner.void() => {
            CoerceResult::VoidPtrCoercion
        }

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

pub fn is_coercible(interner: &mut TypeInterner, from: TypeId, to: TypeId) -> bool {
    try_coerce(interner, from, to).is_ok()
}

pub fn verify_cast(interner: &mut TypeInterner, from: TypeId, to: TypeId) -> bool {
    if is_coercible(interner, from, to) {
        return true;
    }

    let from_ty = interner.get(from);
    let to_ty = interner.get(to);

    let is_numeric = |t: &Type| {
        matches!(t, Type::IntLiteral | Type::FloatLiteral)
            || matches!(t, Type::Builtin(b) if builtin_is_integer(*b) || builtin_is_float(*b))
    };

    let is_bool = |t: &Type| matches!(t, Type::Builtin(BuiltinType::bool));
    let is_char = |t: &Type| matches!(t, Type::Builtin(BuiltinType::char));
    let is_int_only = |t: &Type| {
        matches!(t, Type::IntLiteral) || matches!(t, Type::Builtin(b) if builtin_is_integer(*b))
    };
    let is_pointer = |t: &Type| matches!(t, Type::Pointer { .. } | Type::ManyPointer { .. });

    match (from_ty, to_ty) {
        // numeric <-> numeric
        (a, b) if is_numeric(a) && is_numeric(b) => true,

        // bool <-> integer
        (a, b) if (is_bool(a) && is_int_only(b)) || (is_int_only(a) && is_bool(b)) => true,

        // char <-> integer
        (a, b) if (is_char(a) && is_int_only(b)) || (is_int_only(a) && is_char(b)) => true,

        // enum <-> integer
        (Type::Enum { .. }, b) if is_int_only(b) => true,
        (a, Type::Enum { .. }) if is_int_only(a) => true,

        // ptr <-> ptr
        (a, b) if is_pointer(a) && is_pointer(b) => true,

        // [N]T -> [*]T
        (Type::Array { .. }, Type::ManyPointer { .. }) => true,

        _ => false,
    }
}
