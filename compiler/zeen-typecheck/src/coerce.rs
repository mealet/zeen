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

#[cfg(test)]
mod tests {
    use super::*;

    fn coerce(it: &mut TypeInterner, from: Type, to: Type) -> CoerceResult {
        let from = it.intern(from);
        let to = it.intern(to);
        try_coerce(it, from, to)
    }

    fn builtin(b: BuiltinType) -> Type {
        Type::Builtin(b)
    }

    #[test]
    fn builtin_is_integer_signed_and_unsigned() {
        for b in [
            BuiltinType::i8,
            BuiltinType::i16,
            BuiltinType::i32,
            BuiltinType::i64,
            BuiltinType::isize,
            BuiltinType::u8,
            BuiltinType::u16,
            BuiltinType::u32,
            BuiltinType::u64,
            BuiltinType::usize,
        ] {
            assert!(builtin_is_integer(b));
        }
    }

    #[test]
    fn builtin_is_integer_false_for_non_integers() {
        assert!(!builtin_is_integer(BuiltinType::f32));
        assert!(!builtin_is_integer(BuiltinType::f64));
        assert!(!builtin_is_integer(BuiltinType::bool));
        assert!(!builtin_is_integer(BuiltinType::char));
        assert!(!builtin_is_integer(BuiltinType::void));
    }

    #[test]
    fn builtin_is_float_matches_only_floats() {
        assert!(builtin_is_float(BuiltinType::f32));
        assert!(builtin_is_float(BuiltinType::f64));
        assert!(!builtin_is_float(BuiltinType::i32));
        assert!(!builtin_is_float(BuiltinType::u64));
        assert!(!builtin_is_float(BuiltinType::bool));
    }

    #[test]
    fn coerce_identity_for_equal_types() {
        let mut it = TypeInterner::new();
        let i32 = it.intern(builtin(BuiltinType::i32));

        assert_eq!(try_coerce(&mut it, i32, i32), CoerceResult::Identity);
        let void = it.void();
        assert_eq!(try_coerce(&mut it, void, void), CoerceResult::Identity);
    }

    #[test]
    fn coerce_structural_identity() {
        let mut it = TypeInterner::new();
        let inner = it.intern(builtin(BuiltinType::i32));
        let a = it.intern(Type::Pointer {
            inner,
            is_const: true,
        });
        let b = it.intern(Type::Pointer {
            inner,
            is_const: true,
        });

        assert_eq!(try_coerce(&mut it, a, b), CoerceResult::Identity);
    }

    #[test]
    fn coerce_error_recovers_on_error_type() {
        let mut it = TypeInterner::new();
        let i32 = it.intern(builtin(BuiltinType::i32));
        let err = it.error();

        assert_eq!(try_coerce(&mut it, err, i32), CoerceResult::ErrorRecovery);
        assert_eq!(try_coerce(&mut it, i32, err), CoerceResult::ErrorRecovery);
    }

    #[test]
    fn coerce_never_flows_to_any() {
        let mut it = TypeInterner::new();
        let i32 = it.intern(builtin(BuiltinType::i32));
        let never = it.never();
        let void = it.void();

        assert_eq!(try_coerce(&mut it, never, i32), CoerceResult::NeverCoercion);
        assert_eq!(
            try_coerce(&mut it, never, void),
            CoerceResult::NeverCoercion
        );
    }

    #[test]
    fn coerce_pins_int_literal_to_integer_builtins() {
        let mut it = TypeInterner::new();

        for b in [
            BuiltinType::i8,
            BuiltinType::i16,
            BuiltinType::i32,
            BuiltinType::i64,
            BuiltinType::isize,
            BuiltinType::u8,
            BuiltinType::u16,
            BuiltinType::u32,
            BuiltinType::u64,
            BuiltinType::usize,
        ] {
            assert_eq!(
                coerce(&mut it, Type::IntLiteral, builtin(b)),
                CoerceResult::PinLiteral
            );
        }
    }

    #[test]
    fn coerce_pins_float_literal_to_float_builtins() {
        let mut it = TypeInterner::new();

        assert_eq!(
            coerce(&mut it, Type::FloatLiteral, builtin(BuiltinType::f32)),
            CoerceResult::PinLiteral
        );
        assert_eq!(
            coerce(&mut it, Type::FloatLiteral, builtin(BuiltinType::f64)),
            CoerceResult::PinLiteral
        );
    }

    #[test]
    fn coerce_does_not_pin_literals_to_wrong_kind() {
        let mut it = TypeInterner::new();

        assert_eq!(
            coerce(&mut it, Type::IntLiteral, builtin(BuiltinType::f64)),
            CoerceResult::Fail
        );
        assert_eq!(
            coerce(&mut it, Type::FloatLiteral, builtin(BuiltinType::i32)),
            CoerceResult::Fail
        );
    }
}
