use std::collections::HashMap;

use zeen_mir::{BlockId, LocalId, LocalKind, MirFunction, MirStatement, Place};
use zeen_resolve::DefId;
use zeen_typecheck::result::TypeCheckResult;
use zeen_types::{Type, TypeId, TypeInterner, VariantPayload};

use crate::state::{FunctionState, LocalState, PartialMoveState, ValueState};

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
        | Type::Pointer { .. }
        | Type::ManyPointer { .. }
        | Type::Fn { .. }
        | Type::Void
        | Type::Never
        | Type::Error => false,

        // `Fn`/`FnOnce` closure values own a heap env block, so their death
        // must `free` it back (a synthesized per-type drop function does it).
        Type::FatFn { .. } => true,

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

        Type::Array { element, .. } => type_needs_drop_impl(interner, typecheck, element, bindings),

        Type::Enum {
            def_id,
            generic_args,
        } => {
            let Some(info) = typecheck.enum_info.get(&def_id) else {
                return false;
            };

            if info.capabalities.has_explicit_drop {
                return true;
            }

            let nested = bind_enum_generics(interner, typecheck, &def_id, &generic_args, bindings);
            info.variants.iter().any(|variant| match variant.payload {
                None => false,
                Some(VariantPayload::Single(ty)) | Some(VariantPayload::Struct(ty)) => {
                    type_needs_drop_impl(interner, typecheck, ty, &nested)
                }
            })
        }

        // A slice is a view (`{ ptr, len }`) over someone else's storage: it
        // owns nothing, so it never requires a drop.
        Type::Slice { .. } => false,

        Type::GenericParam(def) => bindings
            .get(&def)
            .is_some_and(|&bound| type_needs_drop_impl(interner, typecheck, bound, bindings)),

        Type::Interface { .. } | Type::InterfaceSelfPlaceholder(_) => false,
    }
}

/// Expands the drop places of a wholly-live value of type `ty` rooted at
/// `place`, flattening structs that do not implement `Drop` into their fields.
///
/// A struct with an explicit `Drop` implementation is dropped as a whole - the
/// codegen emits the implementation's `drop` call. Any other struct is expanded
/// recursively: each field that (transitively) needs a drop ends up as its own
/// `Drop` place. Arrays/slices stay a single drop of the whole aggregate, since
/// their elements are never moved individually. This front-loading keeps the
/// partially-moved state knowledge here; codegen only ever drops the places it
/// is given and never has to decide which fields are live.
fn expand_live_drops(
    interner: &TypeInterner,
    typecheck: &TypeCheckResult,
    place: &Place,
    ty: TypeId,
    bindings: &HashMap<DefId, TypeId>,
    out: &mut Vec<Place>,
) {
    match interner.get(ty).clone() {
        Type::Struct {
            def_id,
            generic_args,
        } => {
            let Some(info) = typecheck.struct_info.get(&def_id) else {
                return;
            };
            if info.capabalities.has_explicit_drop {
                out.push(place.clone());
                return;
            }
            let nested = bind_type_generics(interner, typecheck, &def_id, &generic_args, bindings);
            for field in &info.fields {
                if !type_needs_drop_impl(interner, typecheck, field.field_ty, &nested) {
                    continue;
                }
                expand_live_drops(
                    interner,
                    typecheck,
                    &place.clone().field(field.field_def),
                    field.field_ty,
                    &nested,
                    out,
                );
            }
        }
        // Substitute a generic parameter before recursing.
        Type::GenericParam(def) => {
            if let Some(&bound) = bindings.get(&def) {
                expand_live_drops(interner, typecheck, place, bound, bindings, out);
            }
        }
        // A fat closure value is dropped as a whole: codegen resolves the
        // per-type drop (a heap env goes back through `free`); values of
        // types without a registered drop function are skipped there.
        Type::FatFn { .. } => out.push(place.clone()),
        // An enum dies as a whole: the synthesized per-type drop function
        // switches on the runtime tag and tears down the live payload.
        Type::Enum { .. } => out.push(place.clone()),
        Type::Array { .. } | Type::Slice { .. } => out.push(place.clone()),
        _ => {}
    }
}

/// Expands the drop places of a partially moved struct: only the fields that
/// are still live are dropped, each expanded to its own (possibly nested)
/// explicit drops. Partially moved structs without an explicit `Drop` impl are
/// the only ones that get here - field moves are rejected for `Drop` structs.
fn expand_partial_drops(
    interner: &TypeInterner,
    typecheck: &TypeCheckResult,
    root: &Place,
    ty: TypeId,
    partial: &PartialMoveState,
    out: &mut Vec<Place>,
) {
    match interner.get(ty).clone() {
        Type::Struct {
            def_id,
            generic_args,
        } => {
            let Some(info) = typecheck.struct_info.get(&def_id) else {
                return;
            };
            if info.capabalities.has_explicit_drop {
                return;
            }
            let nested = bind_type_generics(
                interner,
                typecheck,
                &def_id,
                &generic_args,
                &HashMap::default(),
            );
            for field in &info.fields {
                if partial.field(field.field_def) != ValueState::Initialized {
                    continue;
                }
                if !type_needs_drop_impl(interner, typecheck, field.field_ty, &nested) {
                    continue;
                }
                expand_live_drops(
                    interner,
                    typecheck,
                    &root.clone().field(field.field_def),
                    field.field_ty,
                    &nested,
                    out,
                );
            }
        }
        // Not a resolvable struct: fall back to the directly tracked live
        // fields.
        _ => {
            for (field, state) in partial.fields() {
                if *state == ValueState::Initialized {
                    out.push(root.clone().field(*field));
                }
            }
        }
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
    bind_params(interner, params, generic_args, bindings)
}

/// Same as [`bind_type_generics`], but for an enum's generic parameters.
fn bind_enum_generics(
    interner: &TypeInterner,
    typecheck: &TypeCheckResult,
    enum_def: &DefId,
    generic_args: &[TypeId],
    bindings: &HashMap<DefId, TypeId>,
) -> HashMap<DefId, TypeId> {
    let Some(params) = typecheck.enum_generics.get(enum_def) else {
        return bindings.clone();
    };
    bind_params(interner, params, generic_args, bindings)
}

fn bind_params(
    interner: &TypeInterner,
    params: &[DefId],
    generic_args: &[TypeId],
    bindings: &HashMap<DefId, TypeId>,
) -> HashMap<DefId, TypeId> {
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

/// Computes the set of live places that need dropping at scope exit.
///
/// Only values that are still initialized at the point of exit, are not `Copy`
/// and actually need a drop participate in the set. A struct with an explicit
/// `Drop` implementation is dropped as a whole; any other struct is expanded
/// into its live fields (recursively), so codegen never works with a partially
/// moved root.
pub fn collect_scope_drops(
    function: &MirFunction,
    state: &FunctionState,
    interner: &TypeInterner,
    typecheck: &TypeCheckResult,
) -> DropSet {
    let mut drops = DropSet::default();

    for i in 0..function.locals.len() {
        let local = LocalId(i as u32);
        collect_local_drops(function, local, state, interner, typecheck, &mut drops);
    }

    drops
}

/// Adds the drop places of a single local to `drops` if it is live at scope
/// exit. `MaybeInitialized` is *not* dropped here: it is up to the caller to
/// decide whether every path initialized the value (and report an error
/// otherwise).
pub fn collect_local_drops(
    function: &MirFunction,
    local: LocalId,
    state: &FunctionState,
    interner: &TypeInterner,
    typecheck: &TypeCheckResult,
    drops: &mut DropSet,
) {
    let decl = function.local(local);
    if decl.kind == LocalKind::Temporary {
        return;
    }
    if !type_needs_drop(interner, typecheck, decl.ty) {
        return;
    }

    match state.state_of(local) {
        LocalState::Whole(ValueState::Initialized) => {
            expand_live_drops(
                interner,
                typecheck,
                &Place::from_local(local),
                decl.ty,
                &HashMap::default(),
                &mut drops.places,
            );
        }
        LocalState::PartiallyMoved(partial) => {
            expand_partial_drops(
                interner,
                typecheck,
                &Place::from_local(local),
                decl.ty,
                &partial,
                &mut drops.places,
            );
        }
        LocalState::Whole(ValueState::Uninitialized)
        | LocalState::Whole(ValueState::Moved)
        | LocalState::Whole(ValueState::MaybeMoved)
        | LocalState::Whole(ValueState::MaybeInitialized) => {}
    }
}

/// Appends `MirStatement::Drop` statements before the terminator of a specific
/// exit block, in reverse drop order (last declared first).
pub fn insert_drops(function: &mut MirFunction, block: BlockId, drops: &DropSet) {
    let block = function.block_mut(block);
    for place in drops.places.iter().rev() {
        block.statements.push(MirStatement::Drop(place.clone()));
    }
}

/// Inserts `MirStatement::Drop` statements before the statement at `index` in
/// the given block, in reverse drop order (last declared first).
pub fn insert_scope_drop(
    function: &mut MirFunction,
    block: BlockId,
    index: usize,
    drops: &DropSet,
) {
    let block = function.block_mut(block);
    for (offset, place) in drops.places.iter().rev().enumerate() {
        block
            .statements
            .insert(index + offset, MirStatement::Drop(place.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use lasso::Rodeo;
    use zeen_typecheck::result::TypeCheckResult;
    use zeen_types::{
        Capabilities, EnumTypeInfo, EnumVariantInfo, StructFieldInfo, StructTypeInfo,
    };

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

    const RES: DefId = DefId(30);
    const OPT: DefId = DefId(31);
    const NONE_VARIANT: DefId = DefId(32);
    const OWNED_VARIANT: DefId = DefId(33);
    const SOME_VARIANT: DefId = DefId(34);

    /// `Resource { none, owned: Foo }` (Foo has `Drop`) needs a drop through
    /// its payload; `Opt { none, some: i32 }` does not.
    fn enum_scene() -> (TypeInterner, TypeCheckResult) {
        let mut interner = TypeInterner::new();
        let mut rodeo = Rodeo::default();

        let foo = interner.intern(Type::Struct {
            def_id: FOO,
            generic_args: vec![],
        });
        let i32 = interner.intern(Type::Builtin(zeen_ast::types::BuiltinType::i32));

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
        typecheck.enum_info.insert(
            RES,
            EnumTypeInfo {
                def_id: RES,
                variants: vec![
                    EnumVariantInfo {
                        def_id: NONE_VARIANT,
                        name: rodeo.get_or_intern("none"),
                        payload: None,
                    },
                    EnumVariantInfo {
                        def_id: OWNED_VARIANT,
                        name: rodeo.get_or_intern("owned"),
                        payload: Some(VariantPayload::Single(foo)),
                    },
                ],
                capabalities: Capabilities::MOVE_ONLY,
            },
        );
        typecheck.enum_info.insert(
            OPT,
            EnumTypeInfo {
                def_id: OPT,
                variants: vec![
                    EnumVariantInfo {
                        def_id: NONE_VARIANT,
                        name: rodeo.get_or_intern("none"),
                        payload: None,
                    },
                    EnumVariantInfo {
                        def_id: SOME_VARIANT,
                        name: rodeo.get_or_intern("some"),
                        payload: Some(VariantPayload::Single(i32)),
                    },
                ],
                capabalities: Capabilities {
                    is_copy: true,
                    has_explicit_drop: false,
                },
            },
        );

        (interner, typecheck)
    }

    #[test]
    fn enum_with_drop_payload_needs_drop() {
        let (mut interner, typecheck) = enum_scene();
        let res = interner.intern(Type::Enum {
            def_id: RES,
            generic_args: vec![],
        });

        assert!(type_needs_drop(&interner, &typecheck, res));
    }

    #[test]
    fn enum_with_copy_payload_needs_no_drop() {
        let (mut interner, typecheck) = enum_scene();
        let opt = interner.intern(Type::Enum {
            def_id: OPT,
            generic_args: vec![],
        });

        assert!(!type_needs_drop(&interner, &typecheck, opt));
    }

    #[test]
    fn enum_with_explicit_drop_needs_drop() {
        let (mut interner, mut typecheck) = enum_scene();
        typecheck
            .enum_info
            .get_mut(&OPT)
            .expect("opt enum present")
            .capabalities
            .has_explicit_drop = true;
        let opt = interner.intern(Type::Enum {
            def_id: OPT,
            generic_args: vec![],
        });

        assert!(type_needs_drop(&interner, &typecheck, opt));
    }

    #[test]
    fn generic_enum_resolves_payload_param() {
        const GEN_OPT: DefId = DefId(40);
        const GEN_T: DefId = DefId(41);
        const GEN_SOME: DefId = DefId(42);

        let mut interner = TypeInterner::new();
        let mut rodeo = Rodeo::default();

        let foo = interner.intern(Type::Struct {
            def_id: FOO,
            generic_args: vec![],
        });
        let i32 = interner.intern(Type::Builtin(zeen_ast::types::BuiltinType::i32));
        let t = interner.intern(Type::GenericParam(GEN_T));

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
        typecheck.enum_info.insert(
            GEN_OPT,
            EnumTypeInfo {
                def_id: GEN_OPT,
                variants: vec![EnumVariantInfo {
                    def_id: GEN_SOME,
                    name: rodeo.get_or_intern("some"),
                    payload: Some(VariantPayload::Single(t)),
                }],
                capabalities: Capabilities::MOVE_ONLY,
            },
        );
        typecheck.enum_generics.insert(GEN_OPT, vec![GEN_T]);

        let with_foo = interner.intern(Type::Enum {
            def_id: GEN_OPT,
            generic_args: vec![foo],
        });
        let with_i32 = interner.intern(Type::Enum {
            def_id: GEN_OPT,
            generic_args: vec![i32],
        });

        assert!(type_needs_drop(&interner, &typecheck, with_foo));
        assert!(!type_needs_drop(&interner, &typecheck, with_i32));
    }
}
