use std::collections::HashMap;

use zeen_mir::{LocalId, Place};
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
    pub fn field(&self, field: DefId) -> ValueState {
        self.fields
            .get(&field)
            .copied()
            .unwrap_or(ValueState::Uninitialized)
    }

    pub fn set_field(&mut self, field: DefId, state: ValueState) {
        self.fields.insert(field, state);
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
    pub fn read_place(&self, _place: &Place) -> ReadOutcome {
        todo!("inspect local state and field projections")
    }

    /// Writes a new value into a place, resetting it back to initialized.
    pub fn write_place(&mut self, _place: &Place) {
        todo!("set whole local or a single field back to initialized")
    }

    /// Moves a value out of a place (field move or whole-value move).
    pub fn move_place(&mut self, _place: &Place) {
        todo!("mark whole local or a single field as moved")
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
    pub fn merge(&mut self, _other: &Self) {
        todo!("join per-local states, producing maybe-states on disagreement")
    }

    /// Resets the state (fresh function entry).
    pub fn clear(&mut self) {
        self.locals.clear();
        self.borrowed.clear();
    }
}
