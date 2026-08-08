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
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PartialMoveState {
    fields: HashMap<DefId, ValueState>,
}

impl PartialMoveState {
    /// State of a single field. Fields not explicitly tracked are treated as
    /// live (`Initialized`): a struct only becomes "partially moved" once a
    /// field is actually moved, and every other field was live by construction.
    pub fn field(&self, field: DefId) -> ValueState {
        self.fields
            .get(&field)
            .copied()
            .unwrap_or(ValueState::Initialized)
    }

    pub fn set_field(&mut self, field: DefId, state: ValueState) {
        self.fields.insert(field, state);
    }

    /// Borrows the tracked fields, used by drop insertion.
    pub fn fields(&self) -> impl Iterator<Item = (&DefId, &ValueState)> {
        self.fields.iter()
    }

    /// Whether every tracked field is back to initialized. Untracked fields are
    /// live by construction, so this is the signal that a partially moved
    /// struct is whole again and can be used as a whole value.
    pub fn all_fields_initialized(&self) -> bool {
        self.fields.values().all(|&s| s == ValueState::Initialized)
    }
}

/// State of a single local root: a simple state, or a per-field split
/// produced by partially moving a struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalState {
    Whole(ValueState),
    PartiallyMoved(PartialMoveState),
}

/// Kind of an outstanding borrow of a local.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorrowKind {
    Mut,
    Const,
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
    borrowed: HashMap<LocalId, BorrowKind>,
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
                // A moved value is reconstructed field-by-field. Only the written
                // field is live again; the others are uninitialized until written.
                let mut partial = PartialMoveState::default();
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

    /// Records a borrow of a local.
    pub fn borrow_place(&mut self, local: LocalId, kind: BorrowKind) {
        self.borrowed.insert(local, kind);
    }

    /// Kind of outstanding borrow of a local, if any.
    pub fn borrow_of(&self, local: LocalId) -> Option<BorrowKind> {
        self.borrowed.get(&local).copied()
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

        let mut borrowed_keys: Vec<LocalId> = self.borrowed.keys().copied().collect();
        for local in other.borrowed.keys().copied() {
            if !borrowed_keys.contains(&local) {
                borrowed_keys.push(local);
            }
        }
        for local in borrowed_keys {
            let left = self.borrowed.get(&local).copied();
            let right = other.borrowed.get(&local).copied();
            match (left, right) {
                (Some(a), Some(b)) => {
                    let kind = match (a, b) {
                        (BorrowKind::Mut, _) | (_, BorrowKind::Mut) => BorrowKind::Mut,
                        (BorrowKind::Const, BorrowKind::Const) => BorrowKind::Const,
                    };
                    if self.borrowed.get(&local) != Some(&kind) {
                        changed = true;
                        self.borrowed.insert(local, kind);
                    }
                }
                (None, None) => {}
                (Some(kind), None) | (None, Some(kind)) => {
                    if self.borrowed.get(&local) != Some(&kind) {
                        changed = true;
                        self.borrowed.insert(local, kind);
                    }
                }
            }
        }

        changed
    }

    /// Resets the state (fresh function entry).
    pub fn clear(&mut self) {
        self.locals.clear();
        self.borrowed.clear();
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
/// to every tracked field, while untracked fields remain live when the whole
/// value is initialized.
fn join_partial_with_whole(partial: &PartialMoveState, whole: ValueState) -> LocalState {
    let mut out = partial.clone();
    for field in partial.fields.keys() {
        let merged = join_value(out.field(*field), whole);
        out.set_field(*field, merged);
    }
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
