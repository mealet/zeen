use std::collections::HashMap;

use zeen_mir::{BlockId, LocalId, LocalKind, MirFunction, MirStatement, Place, PlaceElem};
use zeen_resolve::DefId;
use zeen_typecheck::result::TypeCheckResult;
use zeen_types::{Type, TypeId, TypeInterner};

use crate::state::{FunctionState, LocalState, ValueState};

/// Places that need a `Drop` statement at scope exit.
#[derive(Debug, Default)]
pub struct DropSet {
    pub places: Vec<Place>,
}

/// Whether a value of type `ty` must be dropped: structs implementing `Drop`,
/// or aggregates (arrays/slices/structs) containing drop-typed values.
///
/// For structs the monomorphized `generic_args` are substituted into the field
/// types before recursing, so a `Pair[Foo, Foo]` whose fields are (transitively)
/// drop-typed counts even though its fields are declared as `T`/`U`.
pub fn type_needs_drop(interner: &TypeInterner, typecheck: &TypeCheckResult, ty: TypeId) -> bool {
    type_needs_drop_impl(interner, typecheck, ty, &HashMap::default())
}

/// Recursive implementation. `bindings` maps the generic parameter `DefId`s of
/// the instantiation being inspected to the concrete types they were
/// substituted with, letting field types be resolved against the real args.
fn type_needs_drop_impl(
    interner: &TypeInterner,
    typecheck: &TypeCheckResult,
    ty: TypeId,
    bindings: &HashMap<DefId, TypeId>,
) -> bool {
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

        Type::Struct {
            def_id,
            generic_args,
        } => {
            let Some(info) = typecheck.struct_info.get(&def_id) else {
                return false;
            };

            if info.capabalities.has_explicit_drop {
                return true;
            }

            let nested = bind_type_generics(interner, typecheck, &def_id, &generic_args, bindings);
            info.fields
                .iter()
                .any(|field| type_needs_drop_impl(interner, typecheck, field.field_ty, &nested))
        }

        Type::Array { element, .. } | Type::Slice { element, .. } => {
            type_needs_drop_impl(interner, typecheck, element, bindings)
        }

        Type::GenericParam(def) => bindings
            .get(&def)
            .is_some_and(|&bound| type_needs_drop_impl(interner, typecheck, bound, bindings)),

        Type::Interface { .. } | Type::InterfaceSelfPlaceholder(_) => false,
    }
}

/// Extends `bindings` with the generic parameters of `struct_def`, resolved to
/// the concrete `generic_args` of this instantiation. An argument may itself be
/// a `GenericParam` of an outer struct, so it is resolved through `bindings`
/// first.
fn bind_type_generics(
    interner: &TypeInterner,
    typecheck: &TypeCheckResult,
    struct_def: &DefId,
    generic_args: &[TypeId],
    bindings: &HashMap<DefId, TypeId>,
) -> HashMap<DefId, TypeId> {
    let Some(params) = typecheck.struct_generics.get(struct_def) else {
        return bindings.clone();
    };

    let mut nested = bindings.clone();
    for (param, arg) in params.iter().zip(generic_args.iter().copied()) {
        let resolved = match interner.get(arg) {
            Type::GenericParam(def) => bindings.get(def).copied().unwrap_or(arg),
            _ => arg,
        };
        nested.insert(*param, resolved);
    }
    nested
}

/// Field `DefId`s of a (possibly monomorphized) struct type whose concrete,
/// substituted type requires a drop. Used to drop only the live fields of a
/// partially moved struct: tracked fields miss untouched ones, and the declared
/// `field_ty` of a generic struct is a `GenericParam` until the args are
/// substituted.
pub fn struct_drop_fields(
    interner: &TypeInterner,
    typecheck: &TypeCheckResult,
    ty: TypeId,
) -> Vec<DefId> {
    match interner.get(ty).clone() {
        Type::Struct {
            def_id,
            generic_args,
        } => {
            let Some(info) = typecheck.struct_info.get(&def_id) else {
                return Vec::new();
            };
            // Partial moves are rejected for explicit-Drop structs, but guard
            // anyway: dropping fields would bypass the implementation's `drop`.
            if info.capabalities.has_explicit_drop {
                return Vec::new();
            }
            let bindings = bind_type_generics(
                interner,
                typecheck,
                &def_id,
                &generic_args,
                &HashMap::default(),
            );
            info.fields
                .iter()
                .filter(|field| {
                    type_needs_drop_impl(interner, typecheck, field.field_ty, &bindings)
                })
                .map(|field| field.field_def)
                .collect()
        }
        _ => Vec::new(),
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
                // Drop only the fields that are still live. Iterate the
                // struct's own drop-requiring fields: untouched fields are not
                // tracked in `partial` but must still be dropped.
                let drop_fields = struct_drop_fields(interner, typecheck, decl.ty);
                if !drop_fields.is_empty() {
                    for field in drop_fields {
                        if partial.field(field) == ValueState::Initialized {
                            drops.places.push(Place::from_local(local).field(field));
                        }
                    }
                } else {
                    for (field, field_state) in partial.fields() {
                        if *field_state == ValueState::Initialized {
                            drops.places.push(Place::from_local(local).field(*field));
                        }
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

/// Appends `MirStatement::Drop` statements before the terminator of a specific
/// exit block, in reverse drop order (last declared first).
pub fn insert_drops(function: &mut MirFunction, block: BlockId, drops: &DropSet) {
    let block = function.block_mut(block);
    for place in drops.places.iter().rev() {
        block.statements.push(MirStatement::Drop(place.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use lasso::Rodeo;
    use zeen_typecheck::result::TypeCheckResult;
    use zeen_types::{Capabilities, StructFieldInfo, StructTypeInfo};

    const FOO: DefId = DefId(0);
    const PAIR: DefId = DefId(1);
    const T_PARAM: DefId = DefId(2);
    const U_PARAM: DefId = DefId(3);

    fn field(
        rodeo: &mut Rodeo,
        name: &str,
        field_def: DefId,
        field_ty: TypeId,
        struct_def: DefId,
    ) -> StructFieldInfo {
        StructFieldInfo {
            name: rodeo.get_or_intern(name),
            field_def,
            field_ty,
            struct_def,
            is_pub: false,
        }
    }

    /// `Foo` implements `Drop`; `Pair[T, U]` does not but wraps two `Foo`s.
    fn pair_scene() -> (TypeInterner, TypeCheckResult) {
        let mut interner = TypeInterner::new();
        let mut rodeo = Rodeo::default();

        let _foo = interner.intern(Type::Struct {
            def_id: FOO,
            generic_args: vec![],
        });
        let t = interner.intern(Type::GenericParam(T_PARAM));
        let u = interner.intern(Type::GenericParam(U_PARAM));

        let mut typecheck = TypeCheckResult::default();
        typecheck.struct_info.insert(
            FOO,
            StructTypeInfo {
                def_id: FOO,
                fields: vec![],
                capabalities: Capabilities {
                    is_copy: false,
                    has_explicit_drop: true,
                },
            },
        );
        typecheck.struct_info.insert(
            PAIR,
            StructTypeInfo {
                def_id: PAIR,
                fields: vec![
                    field(&mut rodeo, "a", DefId(10), t, PAIR),
                    field(&mut rodeo, "b", DefId(11), u, PAIR),
                ],
                capabalities: Capabilities::MOVE_ONLY,
            },
        );
        typecheck
            .struct_generics
            .insert(PAIR, vec![T_PARAM, U_PARAM]);

        (interner, typecheck)
    }

    #[test]
    fn monomorphized_generic_struct_requires_drop() {
        let (mut interner, typecheck) = pair_scene();
        let foo = interner.intern(Type::Struct {
            def_id: FOO,
            generic_args: vec![],
        });
        let pair = interner.intern(Type::Struct {
            def_id: PAIR,
            generic_args: vec![foo, foo],
        });

        assert!(type_needs_drop(&interner, &typecheck, pair));
    }

    #[test]
    fn generic_struct_with_copy_fields_needs_no_drop() {
        let (mut interner, typecheck) = pair_scene();
        let i32 = interner.intern(Type::Builtin(zeen_ast::types::BuiltinType::i32));
        let pair = interner.intern(Type::Struct {
            def_id: PAIR,
            generic_args: vec![i32, i32],
        });

        assert!(!type_needs_drop(&interner, &typecheck, pair));
    }

    #[test]
    fn nested_generic_struct_resolves_outer_params() {
        const OUTER: DefId = DefId(5);
        const X_PARAM: DefId = DefId(6);

        let mut interner = TypeInterner::new();
        let mut rodeo = Rodeo::default();
        let foo = interner.intern(Type::Struct {
            def_id: FOO,
            generic_args: vec![],
        });
        let x = interner.intern(Type::GenericParam(X_PARAM));
        let nested = interner.intern(Type::Struct {
            def_id: PAIR,
            generic_args: vec![x, x],
        });

        let mut typecheck = TypeCheckResult::default();
        typecheck.struct_info.insert(
            FOO,
            StructTypeInfo {
                def_id: FOO,
                fields: vec![],
                capabalities: Capabilities {
                    is_copy: false,
                    has_explicit_drop: true,
                },
            },
        );
        typecheck.struct_info.insert(
            PAIR,
            StructTypeInfo {
                def_id: PAIR,
                fields: vec![
                    field(&mut rodeo, "a", DefId(10), x, PAIR),
                    field(&mut rodeo, "b", DefId(11), x, PAIR),
                ],
                capabalities: Capabilities::MOVE_ONLY,
            },
        );
        typecheck.struct_info.insert(
            OUTER,
            StructTypeInfo {
                def_id: OUTER,
                fields: vec![field(&mut rodeo, "p", DefId(12), nested, OUTER)],
                capabalities: Capabilities::MOVE_ONLY,
            },
        );
        typecheck.struct_generics.insert(PAIR, vec![X_PARAM]);
        typecheck.struct_generics.insert(OUTER, vec![X_PARAM]);

        let outer = interner.intern(Type::Struct {
            def_id: OUTER,
            generic_args: vec![foo],
        });

        assert!(type_needs_drop(&interner, &typecheck, outer));
    }
}
