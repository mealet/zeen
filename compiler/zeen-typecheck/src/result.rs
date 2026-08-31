use std::collections::HashMap;

use zeen_ast::Source;
use zeen_hir::HirId;
use zeen_resolve::DefId;
use zeen_types::{StructTypeInfo, Type, TypeId, TypeInterner};

use crate::closure_alloc::ClosureAllocKind;
use crate::format_str::FormatChunk;

#[derive(Debug, Default)]
pub struct TypeCheckResult {
    pub main_fn_def: Option<DefId>,
    pub interner: TypeInterner,
    pub expr_types: HashMap<HirId, TypeId>,
    pub def_types: HashMap<DefId, TypeId>,
    pub call_resolutions: HashMap<HirId, CallResolution>,
    pub field_resolutions: HashMap<HirId, DefId>,
    pub operator_resolutions: HashMap<HirId, OperatorResolution>,
    pub struct_info: HashMap<DefId, StructTypeInfo>,
    pub struct_generics: HashMap<DefId, Vec<DefId>>,
    /// Interface names -> their `DefId`, so downstream passes (MIR, flow) can
    /// resolve capabilities like `Copy` per concrete instantiation.
    pub interface_registry: HashMap<String, DefId>,
    pub enum_variants: HashMap<DefId, Vec<DefId>>,
    pub method_owner: HashMap<DefId, DefId>,
    pub const_bindings: HashMap<DefId, bool>,
    pub format_specs: HashMap<HirId, Vec<FormatChunk>>,
    /// Per-closure-site environment allocation decision (key = the closure's
    /// synthetic fn `DefId`), see `closure_alloc`.
    pub closure_allocs: HashMap<DefId, ClosureAllocKind>,

    /// Return expressions of functions whose declared return is a `Fn`/
    /// `FnOnce` bound. The concrete closure type of the return is derived
    /// from these after all bodies have been checked.
    pub fat_return_candidates: HashMap<DefId, Vec<(HirId, Source)>>,

    /// Variable defs whose initializer is a fat (or fat-bound-typed) value;
    /// used to resolve the erased bound annotations down to concrete closure
    /// storage types.
    pub fat_let_values: HashMap<DefId, HirId>,

    /// VarRef expressions whose recorded type is a fat value; used by the
    /// finalization to resolve erased bounds through variables.
    pub fat_value_defs: HashMap<HirId, DefId>,

    /// Resolved concrete return type of a function declared to return a
    /// `Fn`/`FnOnce` bound.
    pub fn_return_fats: HashMap<DefId, TypeId>,

    /// Interface implementations registered per `(struct, interface)`,
    /// including concrete specializations (`implement Display : Box[i32]`).
    pub impl_registry: HashMap<(DefId, DefId), Vec<ImplEntry>>,

    /// Interface method chosen for a struct-typed format argument (`{}` /
    /// `{:?}`), keyed by the argument expression. Recorded so MIR dispatches
    /// to the same implementation the checker picked.
    pub format_arg_resolutions: HashMap<HirId, DefId>,

    /// Every method declared directly inside an `interface` block, mapped to
    /// the interface that owns it (`write_str` -> `StrWriter`). Used by MIR to
    /// dispatch a call made on a bounded generic parameter to the concrete
    /// implementation once the receiver is monomorphized.
    pub interface_method_owners: HashMap<DefId, DefId>,
}

/// A single `implement` block registered for a `(struct, interface)` pair.
#[derive(Debug, Clone)]
pub struct ImplEntry {
    pub methods: Vec<DefId>,
    /// Lowered object slots: `Type::GenericParam` for generic-binding slots,
    /// concrete types for specializations.
    pub object_args: Vec<TypeId>,
    pub is_specialized: bool,
    /// Bounds of the implement's generic parameters
    /// (`implement[T: Display]`): `T` -> the interfaces it requires.
    pub generic_bounds: Vec<(DefId, Vec<DefId>)>,
    /// The implement's generic parameters bound to the struct's generic
    /// slots (`implement[T] Display : Box[T]` -> `T` -> the struct's `T`).
    pub generic_bindings: Vec<(DefId, DefId)>,
}

impl TypeCheckResult {
    pub fn record_expr_type(&mut self, id: HirId, ty: TypeId) {
        self.expr_types.insert(id, ty);
    }

    /// Whether a concrete type is `Copy`, decided per instantiation so that
    /// bounded implementations like `implement[T: Copy] Copy : Option[T]`
    /// only make `Option[T]` copyable when `T` itself is copyable.
    ///
    /// The generic-bound cases mirror `TypeChecker::applicable_impl` but use
    /// this Copy-specific predicate for the bounds, since a builtin is always
    /// Copy even though it does not declare a `Copy` interface impl.
    pub fn is_copy(&self, ty: TypeId) -> bool {
        match self.interner.get(ty).clone() {
            Type::Struct {
                def_id,
                generic_args,
            } => {
                let Some(copy_iface) = self.interface_registry.get("Copy").copied() else {
                    return false;
                };
                self.applicable_copy_impl(def_id, copy_iface, &generic_args)
            }
            Type::Array { element, .. } => self.is_copy(element),
            // `Fn` closure values (all-Copy captures or none) are Copy: the
            // inline environment is duplicated with the value; `FnOnce` owns a
            // non-Copy capture so it is move-only.
            Type::FatFn { once, .. } => !once,
            Type::Slice { .. } => true,
            // Builtins, pointers, fn pointers, enums, void, never, error.
            _ => true,
        }
    }

    fn applicable_copy_impl(
        &self,
        struct_def: DefId,
        copy_iface: DefId,
        generic_args: &[TypeId],
    ) -> bool {
        let Some(entries) = self.impl_registry.get(&(struct_def, copy_iface)) else {
            return false;
        };

        // A concrete specialization always wins.
        if entries
            .iter()
            .any(|e| e.is_specialized && e.object_args == generic_args)
        {
            return true;
        }

        let struct_generics = self
            .struct_generics
            .get(&struct_def)
            .cloned()
            .unwrap_or_default();

        // Bounded generic impls require the concrete args to satisfy their
        // bounds (`implement[T: Copy] Copy : Option[T]`).
        for entry in entries
            .iter()
            .filter(|e| !e.is_specialized && !e.generic_bounds.is_empty())
        {
            let applies = entry.generic_bounds.iter().all(|(imp_g, ifaces)| {
                let Some((_, struct_slot)) =
                    entry.generic_bindings.iter().find(|(g, _)| g == imp_g)
                else {
                    return true;
                };
                let Some(index) = struct_generics.iter().position(|g| g == struct_slot) else {
                    return true;
                };
                let Some(concrete) = generic_args.get(index).copied() else {
                    return true;
                };
                // A Copy bound is satisfied exactly when the concrete argument
                // is itself Copy. Non-Copy bounds are checked against the
                // plain interface satisfaction path.
                ifaces.iter().all(|&iface| {
                    if iface == copy_iface {
                        self.is_copy(concrete)
                    } else {
                        self.satisfies_interface(concrete, iface)
                    }
                })
            });

            if applies {
                return true;
            }
        }

        // A boundless wildcard implementation applies to every instantiation.
        entries
            .iter()
            .any(|e| !e.is_specialized && e.generic_bounds.is_empty())
    }

    /// Whether a concrete (or interface) type satisfies the given interface.
    /// Used to validate non-`Copy` bounds on a `Copy` implementation.
    fn satisfies_interface(&self, ty: TypeId, iface_def: DefId) -> bool {
        match self.interner.get(ty).clone() {
            Type::Error => true,
            Type::Struct {
                def_id,
                generic_args,
            } => self.applicable_interface(def_id, iface_def, &generic_args),
            _ => false,
        }
    }

    /// Selects the applicable implementation for `(struct_def, iface_def)` at
    /// a concrete instantiation, reusing `applicable_copy_impl` for `Copy` so
    /// builtins and bounded `Copy` impls are handled consistently.
    fn applicable_interface(
        &self,
        struct_def: DefId,
        iface_def: DefId,
        generic_args: &[TypeId],
    ) -> bool {
        let Some(entries) = self.impl_registry.get(&(struct_def, iface_def)) else {
            return false;
        };

        if entries
            .iter()
            .any(|e| e.is_specialized && e.object_args == generic_args)
        {
            return true;
        }

        let struct_generics = self
            .struct_generics
            .get(&struct_def)
            .cloned()
            .unwrap_or_default();

        for entry in entries
            .iter()
            .filter(|e| !e.is_specialized && !e.generic_bounds.is_empty())
        {
            let applies = entry.generic_bounds.iter().all(|(imp_g, ifaces)| {
                let Some((_, struct_slot)) =
                    entry.generic_bindings.iter().find(|(g, _)| g == imp_g)
                else {
                    return true;
                };
                let Some(index) = struct_generics.iter().position(|g| g == struct_slot) else {
                    return true;
                };
                let Some(concrete) = generic_args.get(index).copied() else {
                    return true;
                };
                ifaces.iter().all(|&iface| {
                    if iface == iface_def {
                        false
                    } else {
                        self.satisfies_interface(concrete, iface)
                    }
                })
            });

            if applies {
                return true;
            }
        }

        entries
            .iter()
            .any(|e| !e.is_specialized && e.generic_bounds.is_empty())
    }
}

#[derive(Debug)]
pub struct CallResolution {
    pub fn_def: DefId,
    pub generic_args: Vec<TypeId>,
}

#[derive(Debug, Clone)]
pub struct OperatorResolution {
    pub method_def: DefId,
    pub generic_args: Vec<TypeId>,
}
