use zeen_mir::{MirFunction, Place};
use zeen_typecheck::result::TypeCheckResult;
use zeen_types::{Type, TypeId, TypeInterner};

use crate::state::FunctionState;

/// Places that need a `Drop` statement at scope exit.
#[derive(Debug, Default)]
pub struct DropSet {
    pub places: Vec<Place>,
}

/// Whether a value of type `ty` must be dropped: structs implementing `Drop`,
/// or aggregates (arrays/slices) containing drop-typed values.
pub fn type_needs_drop(interner: &TypeInterner, typecheck: &TypeCheckResult, ty: TypeId) -> bool {
    match interner.get(ty).clone() {
        Type::Builtin(_)
        | Type::IntLiteral
        | Type::FloatLiteral
        | Type::Enum { .. }
        | Type::Pointer { .. }
        | Type::ManyPointer { .. }
        | Type::Fn { .. }
        | Type::Void
        | Type::Never
        | Type::Error => false,

        Type::Struct { def_id, .. } => {
            let Some(info) = typecheck.struct_info.get(&def_id) else {
                return false;
            };

            if info.capabalities.has_explicit_drop {
                return true;
            }

            // TODO: substitute `generic_args` into field types before recursing
            info.fields
                .iter()
                .any(|field| type_needs_drop(interner, typecheck, field.field_ty))
        }

        Type::Array { element, .. } | Type::Slice { element, .. } => {
            type_needs_drop(interner, typecheck, element)
        }

        Type::Interface { .. } | Type::GenericParam(_) | Type::InterfaceSelfPlaceholder(_) => false,
    }
}

/// Computes the set of live places that need dropping at scope exit.
///
/// Only values that are still initialized at the point of exit, are not `Copy`
/// and actually need a drop participate in the set.
pub fn collect_scope_drops(
    function: &MirFunction,
    state: &FunctionState,
    interner: &TypeInterner,
    typecheck: &TypeCheckResult,
) -> DropSet {
    // TODO: walk `state` + `function.locals`, filter by
    // - initialized (or re-initialized) local,
    // - `type_needs_drop(local.ty)`,
    // - structs only partially moved: drop remaining live fields, not the root.
    let _ = (function, state, interner, typecheck);
    todo!("collect initialized, non-copy, still-live places from `state`")
}

/// Appends `MirStatement::Drop` before the terminator of exit blocks.
pub fn insert_drops(function: &mut MirFunction, drops: &DropSet) {
    // TODO: for each exit block of `function` (terminator is `Return`),
    // append `MirStatement::Drop(place)` in reverse drop order before it.
    let _ = (function, drops);
    todo!("append Drop statements to exit blocks")
}
