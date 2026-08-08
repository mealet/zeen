use zeen_mir::{LocalId, LocalKind, MirFunction, MirStatement, Place, PlaceElem, Terminator};
use zeen_typecheck::result::TypeCheckResult;
use zeen_types::{Type, TypeId, TypeInterner};

use crate::state::{FunctionState, LocalState, ValueState};

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
/// and actually need a drop participate in the set. Structs that were only
/// partially moved drop their remaining live fields instead of the root.
pub fn collect_scope_drops(
    function: &MirFunction,
    state: &FunctionState,
    interner: &TypeInterner,
    typecheck: &TypeCheckResult,
) -> DropSet {
    let mut drops = DropSet::default();

    for i in 0..function.locals.len() {
        let local = LocalId(i as u32);
        let decl = function.local(local);
        if decl.kind == LocalKind::Temporary {
            continue;
        }
        if !type_needs_drop(interner, typecheck, decl.ty) {
            continue;
        }

        match state.state_of(local) {
            LocalState::Whole(ValueState::Initialized) => {
                drops.places.push(Place::from_local(local));
            }
            LocalState::Whole(ValueState::MaybeInitialized) => {
                // Conditionally live: drop conservatively.
                drops.places.push(Place::from_local(local));
            }
            LocalState::PartiallyMoved(partial) => {
                // Drop only the fields that are still live.
                for (field, field_state) in partial.fields() {
                    if *field_state == ValueState::Initialized {
                        drops.places.push(Place::from_local(local).field(*field));
                    }
                }
            }
            LocalState::Whole(ValueState::Uninitialized)
            | LocalState::Whole(ValueState::Moved)
            | LocalState::Whole(ValueState::MaybeMoved) => {}
        }
    }

    drops
}

/// Appends `MirStatement::Drop` before the terminator of exit blocks, in
/// reverse drop order.
pub fn insert_drops(function: &mut MirFunction, drops: &DropSet) {
    let exit_blocks: Vec<usize> = function
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(i, block)| match &block.terminator {
            Terminator::Return(_) => Some(i),
            _ => None,
        })
        .collect();

    if exit_blocks.is_empty() {
        return;
    }

    for block_index in exit_blocks {
        let block = &mut function.blocks[block_index];
        for place in drops.places.iter().rev() {
            block.statements.push(MirStatement::Drop(place.clone()));
        }
    }
}
