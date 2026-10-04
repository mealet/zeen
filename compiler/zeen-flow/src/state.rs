use std::collections::HashMap;

use zeen_mir::{LocalId, Place, PlaceElem};
use zeen_resolve::DefId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueState {
    Uninitialized,
    Initialized,
    Moved,
    MaybeInitialized,
    MaybeMoved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialMoveState {
    fields: HashMap<DefId, ValueState>,
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
    pub fn of_live() -> Self {
        Self::default()
    }

    pub fn of_rebuild() -> Self {
        Self {
            fields: HashMap::new(),
            untracked: ValueState::Uninitialized,
        }
    }

    pub fn field(&self, field: DefId) -> ValueState {
        self.fields.get(&field).copied().unwrap_or(self.untracked)
    }

    pub fn set_field(&mut self, field: DefId, state: ValueState) {
        self.fields.insert(field, state);
    }

    pub fn fields(&self) -> impl Iterator<Item = (&DefId, &ValueState)> {
        self.fields.iter()
    }

    pub fn all_fields_initialized(&self) -> bool {
        self.untracked == ValueState::Initialized
            && self.fields.values().all(|&s| s == ValueState::Initialized)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalState {
    Whole(ValueState),
    PartiallyMoved(PartialMoveState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOutcome {
    Ok,
    Uninitialized,
    Moved,
    PartiallyMoved,
    MaybeUninitialized,
    MaybeMoved,
}

#[derive(Debug, Clone, Default)]
pub struct FunctionState {
    locals: HashMap<LocalId, LocalState>,
    freed: Vec<Place>,
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

    pub fn reinitialize(&mut self, local: LocalId) {
        self.set_state(local, LocalState::Whole(ValueState::Initialized));
    }

    pub fn mark_moved(&mut self, local: LocalId) {
        self.set_state(local, LocalState::Whole(ValueState::Moved));
    }

    pub fn mark_freed(&mut self, place: Place) {
        if !self.freed.contains(&place) {
            self.freed.push(place);
        };
    }

    pub fn freed_places(&self) -> &[Place] {
        &self.freed
    }

    pub fn read_place(&self, place: &Place) -> ReadOutcome {
        match self.state_of(place.local) {
            LocalState::Whole(state) => read_value_state(state),
            LocalState::PartiallyMoved(partial) => {
                if let Some(first) = first_field(place) {
                    read_value_state(partial.field(first))
                } else if partial.all_fields_initialized() {
                    ReadOutcome::Ok
                } else {
                    ReadOutcome::PartiallyMoved
                }
            }
        }
    }

    pub fn write_place(&mut self, place: &Place) {
        let Some(first) = first_field(place) else {
            self.reinitialize(place.local);
            return;
        };

        match self.state_of(place.local) {
            LocalState::Whole(ValueState::Moved) | LocalState::Whole(ValueState::Uninitialized) => {
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
            LocalState::Whole(_) => self.reinitialize(place.local),
        }
    }

    pub fn write_struct_place(&mut self, place: &Place, all_fields: &[DefId]) {
        let Some(first) = first_field(place) else {
            self.reinitialize(place.local);
            return;
        };

        match self.state_of(place.local) {
            LocalState::Whole(ValueState::Moved) | LocalState::Whole(ValueState::Uninitialized) => {
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
            LocalState::Whole(_) => self.mark_moved(place.local),
        }
    }

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

        let before = self.freed.len();
        self.freed.retain(|place| other.freed.contains(place));
        changed = changed || self.freed.len() != before;

        changed
    }

    pub fn clear(&mut self) {
        self.locals.clear();
        self.freed.clear();
    }
}

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

        (Initialized, Initialized) => Initialized,

        (Uninitialized, Uninitialized) => Uninitialized,

        (Moved, Moved) => Moved,

        (MaybeInitialized, MaybeInitialized) => MaybeInitialized,

        (MaybeMoved, MaybeMoved) => MaybeMoved,
    }
}

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

fn join_partial_with_whole(partial: &PartialMoveState, whole: ValueState) -> LocalState {
    let mut out = partial.clone();
    for field in partial.fields.keys() {
        let merged = join_value(out.field(*field), whole);
        out.set_field(*field, merged);
    }
    out.untracked = join_value(out.untracked, whole);
    if whole == ValueState::Uninitialized && out.all_fields_initialized() {
        out.set_field(
            *partial.fields.keys().next().unwrap_or(&DefId(u32::MAX)),
            ValueState::Uninitialized,
        );
    }
    LocalState::PartiallyMoved(out)
}
