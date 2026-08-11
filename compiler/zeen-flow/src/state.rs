use std::collections::HashMap;

use zeen_mir::{LocalId, Place, PlaceElem};
use zeen_resolve::DefId;

/// Abstract state of a single value at a program point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueState {
    /// Never written before first read.
    Uninitialized,
    /// Written and not moved out.
    Initialized,
    /// Value was moved out of the place.
    Moved,
    /// CFG paths disagree between initialized and uninitialized.
    MaybeInitialized,
    /// CFG paths disagree between initialized and moved.
    MaybeMoved,
}

/// Per-field states of a partially moved struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialMoveState {
    fields: HashMap<DefId, ValueState>,
    /// State of fields not tracked in `fields`. Splitting a fully-initialized
    /// value (a field was moved out) leaves untouched fields live
    /// (`Initialized`); rebuilding a moved/uninitialized value field-by-field
    /// keeps untouched fields dead (`Uninitialized`, so an early whole read is
    /// caught instead of reporting a phantom-initialized struct).
    untracked: ValueState,
}

impl Default for PartialMoveState {
    fn default() -> Self {
        Self {
            fields: HashMap::new(),
            untracked: ValueState::Initialized,
        }
    }
}

impl PartialMoveState {
    /// Creates a partial state from a live value: untouched fields stay live.
    pub fn of_live() -> Self {
        Self::default()
    }

    /// Creates a partial state that is being rebuilt from a moved or
    /// uninitialized value: untouched fields are uninitialized.
    pub fn of_rebuild() -> Self {
        Self {
            fields: HashMap::new(),
            untracked: ValueState::Uninitialized,
        }
    }

    /// State of a single field, defaulting to the untracked state.
    pub fn field(&self, field: DefId) -> ValueState {
        self.fields.get(&field).copied().unwrap_or(self.untracked)
    }

    pub fn set_field(&mut self, field: DefId, state: ValueState) {
        self.fields.insert(field, state);
    }

    /// Borrows the tracked fields, used by drop insertion.
    pub fn fields(&self) -> impl Iterator<Item = (&DefId, &ValueState)> {
        self.fields.iter()
    }

    /// Whether every tracked field is initialized and the untracked default is
    /// live. Only then can the struct be used as a whole value.
    pub fn all_fields_initialized(&self) -> bool {
        self.untracked == ValueState::Initialized
            && self.fields.values().all(|&s| s == ValueState::Initialized)
    }
}

/// State of a single local root: a simple state, or a per-field split
/// produced by partially moving a struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalState {
    Whole(ValueState),
    PartiallyMoved(PartialMoveState),
}

/// Outcome of attempting to read a place as a fully-initialized value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    Ok,
    Uninitialized,
    Moved,
    PartiallyMoved,
    MaybeUninitialized,
    MaybeMoved,
}

/// Snapshot of every local of a function at a single program point.
#[derive(Debug, Clone, Default)]
pub struct FunctionState {
    locals: HashMap<LocalId, LocalState>,
}

impl FunctionState {
    pub fn state_of(&self, local: LocalId) -> LocalState {
        self.locals
            .get(&local)
            .cloned()
            .unwrap_or(LocalState::Whole(ValueState::Uninitialized))
    }

    pub fn set_state(&mut self, local: LocalId, state: LocalState) {
        self.locals.insert(local, state);
    }

    /// Marks a local as initialized, resetting any partial-move split.
    pub fn reinitialize(&mut self, local: LocalId) {
        self.set_state(local, LocalState::Whole(ValueState::Initialized));
    }

    /// Marks a local as fully moved out.
    pub fn mark_moved(&mut self, local: LocalId) {
        self.set_state(local, LocalState::Whole(ValueState::Moved));
    }

    /// Whether a place is safe to read as a fully-initialized value.
    ///
    /// Copy semantics are *not* handled here: the caller is expected to consult
    /// the type at the place and skip reading modelling for `Copy` types.
    pub fn read_place(&self, place: &Place) -> ReadOutcome {
        match self.state_of(place.local) {
            LocalState::Whole(state) => read_value_state(state),
            LocalState::PartiallyMoved(partial) => {
                if let Some(first) = first_field(place) {
                    read_value_state(partial.field(first))
                } else if partial.all_fields_initialized() {
                    // Every tracked field is live again, so the whole value is.
                    ReadOutcome::Ok
                } else {
                    ReadOutcome::PartiallyMoved
                }
            }
        }
    }

    /// Writes a new value into a place, resetting it back to initialized.
    pub fn write_place(&mut self, place: &Place) {
        let Some(first) = first_field(place) else {
            self.reinitialize(place.local);
            return;
        };

        match self.state_of(place.local) {
            LocalState::Whole(ValueState::Moved) | LocalState::Whole(ValueState::Uninitialized) => {
                // The value is rebuilt field-by-field from scratch: only the
                // written field is live again, all others stay uninitialized.
                let mut partial = PartialMoveState::of_rebuild();
                partial.set_field(first, ValueState::Initialized);
                self.set_state(place.local, LocalState::PartiallyMoved(partial));
            }
            LocalState::PartiallyMoved(mut partial) => {
                partial.set_field(first, ValueState::Initialized);
                if partial.all_fields_initialized() {
                    self.reinitialize(place.local);
                } else {
                    self.set_state(place.local, LocalState::PartiallyMoved(partial));
                }
            }
            // Writing into a whole-, already-initialized value keeps it intact.
            LocalState::Whole(_) => self.reinitialize(place.local),
        }
    }

    /// Writes a value into a field with knowledge of the struct's whole field
    /// set. A struct rebuilt out of a moved/uninitialized value only becomes
    /// a whole, usable value once every field has been written; meanwhile each
    /// field is tracked explicitly.
    pub fn write_struct_place(&mut self, place: &Place, all_fields: &[DefId]) {
        let Some(first) = first_field(place) else {
            self.reinitialize(place.local);
            return;
        };

        match self.state_of(place.local) {
            LocalState::Whole(ValueState::Moved) | LocalState::Whole(ValueState::Uninitialized) => {
                // Every field of the struct is enumerated, so no untracked
                // default applies; each field starts dead until written.
                let mut partial = PartialMoveState::default();
                for &field in all_fields {
                    partial.set_field(field, ValueState::Uninitialized);
                }
                partial.set_field(first, ValueState::Initialized);
                self.set_state(place.local, LocalState::PartiallyMoved(partial));
            }
            LocalState::PartiallyMoved(mut partial) => {
                partial.set_field(first, ValueState::Initialized);
                if partial.all_fields_initialized() {
                    self.reinitialize(place.local);
                } else {
                    self.set_state(place.local, LocalState::PartiallyMoved(partial));
                }
            }
            LocalState::Whole(_) => self.reinitialize(place.local),
        }
    }

    /// Moves a value out of a place (field move or whole-value move).
    ///
    /// The caller must have already validated the read via [`Self::read_place`];
    /// this performs the state transition only.
    pub fn move_place(&mut self, place: &Place) {
        let Some(first) = first_field(place) else {
            self.mark_moved(place.local);
            return;
        };

        match self.state_of(place.local) {
            LocalState::Whole(ValueState::Initialized) => {
                let mut partial = PartialMoveState::default();
                partial.set_field(first, ValueState::Moved);
                self.set_state(place.local, LocalState::PartiallyMoved(partial));
            }
            LocalState::PartiallyMoved(mut partial) => {
                partial.set_field(first, ValueState::Moved);
                self.set_state(place.local, LocalState::PartiallyMoved(partial));
            }
            // Moved/uninitialized roots were already flagged by the read check.
            LocalState::Whole(_) => self.mark_moved(place.local),
        }
    }

    /// Merges another state into this one (join of the dataflow lattice),
    /// producing maybe-* states when the two disagree.
    pub fn merge(&mut self, other: &Self) -> bool {
        let mut keys: Vec<LocalId> = self.locals.keys().copied().collect();
        for local in other.locals.keys().copied() {
            if !keys.contains(&local) {
                keys.push(local);
            }
        }

        let mut changed = false;
        for local in keys {
            let left = self
                .locals
                .get(&local)
                .cloned()
                .unwrap_or(LocalState::Whole(ValueState::Uninitialized));
            let right = other
                .locals
                .get(&local)
                .cloned()
                .unwrap_or(LocalState::Whole(ValueState::Uninitialized));

            let joined = join_local(local, &left, &right);
            if joined != left {
                changed = true;
                self.locals.insert(local, joined);
            }
        }

        changed
    }

    /// Resets the state (fresh function entry).
    pub fn clear(&mut self) {
        self.locals.clear();
    }
}

/// First `Field` projection of a place, if any. Non-field projections
/// (deref/index/...) fall back to whole-local semantics.
fn first_field(place: &Place) -> Option<DefId> {
    match place.projection.first() {
        Some(PlaceElem::Field(field)) => Some(*field),
        _ => None,
    }
}

fn read_value_state(state: ValueState) -> ReadOutcome {
    match state {
        ValueState::Initialized => ReadOutcome::Ok,
        ValueState::Uninitialized => ReadOutcome::Uninitialized,
        ValueState::Moved => ReadOutcome::Moved,
        ValueState::MaybeInitialized => ReadOutcome::MaybeUninitialized,
        ValueState::MaybeMoved => ReadOutcome::MaybeMoved,
    }
}

/// Join of two value states on the lattice.
///
/// `Initialized` and `Uninitialized` disagree -> `MaybeInitialized`;
/// `Initialized` and `Moved` disagree -> `MaybeMoved`. The maybe-* states are
/// top in their branch and win over the concrete states, so the join is
/// monotone and the worklist terminates.
fn join_value(left: ValueState, right: ValueState) -> ValueState {
    use ValueState::*;
    if left == right {
        return left;
    }
    match (left, right) {
        (Initialized, Uninitialized) | (Uninitialized, Initialized) => MaybeInitialized,
        (Initialized, Moved)
        | (Moved, Initialized)
        | (Uninitialized, Moved)
        | (Moved, Uninitialized) => MaybeMoved,
        (Initialized, MaybeInitialized)
        | (MaybeInitialized, Initialized)
        | (Uninitialized, MaybeInitialized)
        | (MaybeInitialized, Uninitialized)
        | (Moved, MaybeInitialized)
        | (MaybeInitialized, Moved)
        | (MaybeInitialized, MaybeMoved)
        | (MaybeMoved, MaybeInitialized) => MaybeInitialized,
        (Initialized, MaybeMoved)
        | (MaybeMoved, Initialized)
        | (Uninitialized, MaybeMoved)
        | (MaybeMoved, Uninitialized)
        | (Moved, MaybeMoved)
        | (MaybeMoved, Moved) => MaybeMoved,
        // Equal pairs are unreachable (early return above), but exhaustiveness
        // still requires them.
        (Initialized, Initialized) => Initialized,
        (Uninitialized, Uninitialized) => Uninitialized,
        (Moved, Moved) => Moved,
        (MaybeInitialized, MaybeInitialized) => MaybeInitialized,
        (MaybeMoved, MaybeMoved) => MaybeMoved,
    }
}

/// Join of two local states, keeping per-field splits where either side has
/// one.
fn join_local(_local: LocalId, left: &LocalState, right: &LocalState) -> LocalState {
    match (left, right) {
        (LocalState::Whole(a), LocalState::Whole(b)) => LocalState::Whole(join_value(*a, *b)),
        (LocalState::PartiallyMoved(a), LocalState::PartiallyMoved(b)) => {
            let mut out = a.clone();
            for (field, state) in &b.fields {
                let joined = join_value(out.field(*field), *state);
                out.set_field(*field, joined);
            }
            LocalState::PartiallyMoved(out)
        }
        (LocalState::PartiallyMoved(a), LocalState::Whole(b)) => join_partial_with_whole(a, *b),
        (LocalState::Whole(b), LocalState::PartiallyMoved(a)) => join_partial_with_whole(a, *b),
    }
}

/// Joins a per-field split with a whole value: the whole value's state applies
/// to every tracked field, while the untracked default is joined separately.
fn join_partial_with_whole(partial: &PartialMoveState, whole: ValueState) -> LocalState {
    let mut out = partial.clone();
    for field in partial.fields.keys() {
        let merged = join_value(out.field(*field), whole);
        out.set_field(*field, merged);
    }
    out.untracked = join_value(out.untracked, whole);
    if whole == ValueState::Uninitialized && out.all_fields_initialized() {
        // Untracked fields are live by default; pin one so the split isn't
        // mistaken for a whole, live value.
        out.set_field(
            *partial.fields.keys().next().unwrap_or(&DefId(u32::MAX)),
            ValueState::Uninitialized,
        );
    }
    LocalState::PartiallyMoved(out)
}
