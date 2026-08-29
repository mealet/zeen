use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use lasso::{Rodeo, Spur};
use miette::{Diagnostic, Severity};
use smol_str::SmolStr;
use zeen_ast::Source;
use zeen_mir::{
    BasicBlock, BlockId, CallTarget, LocalId, LocalKind, MirFunctionId, MirProgram, MirStatement,
    Operand, Place, PlaceElem, Rvalue, Terminator, place_is_global,
};
use zeen_resolve::{DefId, ResolutionResult};
use zeen_typecheck::result::TypeCheckResult;
use zeen_types::{Type, TypeId};

use crate::{
    drop::{self, DropSet},
    error::FlowError,
    result::FlowResult,
    state::{FunctionState, LocalState, ReadOutcome, ValueState},
};

/// Owned snapshot of a function, taken so the analysis never borrows the
/// program while it also mutates its own bookkeeping.
struct FunctionSnapshot {
    entry_block: BlockId,
    params: Vec<LocalId>,
    locals: Vec<LocalInfo>,
    blocks: Vec<BasicBlock>,
}

struct LocalInfo {
    ty: TypeId,
    kind: LocalKind,
    name: Option<SmolStr>,
    source: Option<Source>,
}

/// The dataflow pass over a lowered MIR program.
///
/// Consumes a mutable `MirProgram`, analyzes every function's CFG, reports
/// move/init diagnostics and inserts `Drop` statements where needed.
pub struct DataFlow<'ctx> {
    program: &'ctx mut MirProgram,
    typecheck: &'ctx mut TypeCheckResult,
    rodeo: Rc<RefCell<Rodeo>>,

    /// In-progress state of the currently analyzed function.
    current: FunctionState,
    /// Merged in-states of each block, used by the worklist.
    block_in_states: HashMap<BlockId, FunctionState>,
    /// States at the `Return` terminators, keyed by the returning block. Drop
    /// insertion looks up the state of the exact exit block, so different exits
    /// of a function keep their own live-set (a value live on one early-return
    /// path must not be dropped on a path where it was already moved).
    exit_states: HashMap<BlockId, FunctionState>,
    /// States prior to each `StorageDead`, keyed by `(block, statment index)`
    /// with the local being ended. Scope-exit drop insertion uses these to
    /// drop exactly the locals that are still live when a block scope ends.
    storage_states: HashMap<(BlockId, usize), FunctionState>,
    /// Locals read anywhere in the current function, for unused warnings.
    read_locals: HashSet<LocalId>,
    /// Snapshot of the function currently being analyzed.
    snapshot: Option<FunctionSnapshot>,
    /// Source of the statement/terminator currently being processed, used to
    /// point borrow/move diagnostics at the offending use site.
    current_source: Option<Source>,
    /// Block currently being analysed, used to record exit states.
    active_block: BlockId,

    diagnostics: Vec<FlowError>,
    functions_with_drops: Vec<MirFunctionId>,
}

impl<'ctx> DataFlow<'ctx> {
    pub fn new(
        program: &'ctx mut MirProgram,
        typecheck: &'ctx mut TypeCheckResult,
        _resolution: &'ctx ResolutionResult,
        rodeo: Rc<RefCell<Rodeo>>,
    ) -> Self {
        Self {
            program,
            typecheck,
            rodeo,
            current: FunctionState::default(),
            block_in_states: HashMap::new(),
            exit_states: HashMap::new(),
            storage_states: HashMap::new(),
            read_locals: HashSet::new(),
            snapshot: None,
            current_source: None,
            active_block: BlockId(0),
            diagnostics: Vec::new(),
            functions_with_drops: Vec::new(),
        }
    }

    /// Runs the whole pass over every function of the program.
    pub fn run(&mut self) {
        let function_ids: Vec<MirFunctionId> = self.program.functions.keys().copied().collect();

        for function_id in function_ids {
            self.analyze_function(function_id);
        }

        self.check_escaping_borrows();
    }

    /// Dataflow over a single function, ending with drop insertion
    /// into its exit blocks.
    fn analyze_function(&mut self, function_id: MirFunctionId) {
        self.current.clear();
        self.block_in_states.clear();
        self.exit_states.clear();
        self.storage_states.clear();
        self.read_locals.clear();

        // Take an owned snapshot of the function's shapes.
        let snapshot = {
            let function = self.program.functions.get(&function_id).unwrap();
            FunctionSnapshot {
                entry_block: function.entry_block,
                params: function.params.clone(),
                locals: function
                    .locals
                    .iter()
                    .map(|decl| LocalInfo {
                        ty: decl.ty,
                        kind: decl.kind,
                        name: decl.name.map(|name| self.resolve_name(name)),
                        source: decl.source.clone(),
                    })
                    .collect(),
                blocks: function.blocks.clone(),
            }
        };
        self.snapshot = Some(snapshot);

        // Entry state: parameters are live, everything else is uninitialized.
        let snapshot = self.snapshot.as_ref().unwrap();

        // Owned copies for the worklist, so the loop never borrows `self`
        // while the statement transfer takes `&mut self`.
        let entry_block = snapshot.entry_block;
        let blocks = snapshot.blocks.clone();
        let params = snapshot.params.clone();
        for &param in &params {
            self.current.reinitialize(param);
        }

        let successors = compute_successors(snapshot);

        self.block_in_states
            .insert(entry_block, self.current.clone());

        let mut worklist = vec![entry_block];
        while let Some(block_id) = worklist.pop() {
            self.current = self
                .block_in_states
                .get(&block_id)
                .cloned()
                .unwrap_or_default();

            self.active_block = block_id;
            let block = &blocks[block_id.0 as usize];
            self.current_source = None;
            for (stmt_index, stmt) in block.statements.iter().enumerate() {
                self.apply_statement(stmt, stmt_index);
            }

            self.apply_terminator(&block.terminator);

            let after = self.current.clone();
            let succ_ids = successors.get(&block_id).cloned().unwrap_or_default();
            for succ in succ_ids {
                // A block with no incoming edge yet adopts this edge's state
                // outright; joining would mix it with the bottom
                // (all-uninitialized) default and poison every first visit.
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    self.block_in_states.entry(succ)
                {
                    entry.insert(after.clone());
                    worklist.push(succ);
                    continue;
                }
                let merged = self.block_in_states.get_mut(&succ).unwrap();
                if merged.merge(&after) {
                    worklist.push(succ);
                }
            }
        }

        self.report_unused();

        self.insert_scope_drops(function_id);
        self.insert_drops(function_id);
    }

    /// Applies a statement's effect to `self.current`.
    fn apply_statement(&mut self, stmt: &MirStatement, stmt_index: usize) {
        match stmt {
            MirStatement::Assign {
                place,
                rvalue,
                source,
            } => {
                self.current_source = source.clone();
                match rvalue {
                    Rvalue::Use(operand) => self.consume_operand(operand),
                    Rvalue::BinaryOp { lhs, rhs, .. } => {
                        self.consume_operand(lhs);
                        self.consume_operand(rhs);
                    }
                    Rvalue::UnaryOp { operand, .. } => self.consume_operand(operand),
                    Rvalue::Cast { operand, .. } => self.consume_operand(operand),
                    Rvalue::Ref {
                        place: ref_place, ..
                    } => {
                        self.consume_copy_place(ref_place, None);
                    }
                    Rvalue::Aggregate { operands, .. } => {
                        for operand in operands {
                            self.consume_operand(operand);
                        }
                    }
                    Rvalue::Discriminant(place) => self.consume_copy_place(place, None),
                    Rvalue::SizeOf(_) | Rvalue::AlignOf(_) => {}
                }

                self.write_destination(place);
            }
            MirStatement::Drop(place) => {
                self.current_source = None;
                self.consume_move_place(place, None);
            }
            MirStatement::Discard(operand) => {
                self.current_source = None;
                self.consume_operand(operand);
            }
            MirStatement::StorageLive(local) => {
                self.current_source = None;
                self.current
                    .set_state(*local, LocalState::Whole(ValueState::Uninitialized));
            }
            MirStatement::StorageDead(local) => {
                self.storage_states
                    .insert((self.active_block, stmt_index), self.current.clone());
                self.current_source = None;
                self.current.mark_moved(*local);
            }
            MirStatement::Nop => {
                self.current_source = None;
            }
        }
    }

    /// Applies a terminator's effect: consumes operands, writes the call
    /// destination, and records the block's state on `Return`.
    fn apply_terminator(&mut self, terminator: &Terminator) {
        match terminator {
            Terminator::Goto(_) | Terminator::Unreachable => {
                self.current_source = None;
            }
            Terminator::SwitchInt { discriminant, .. } => {
                self.consume_operand(discriminant);
                self.current_source = None;
            }
            Terminator::Call {
                func,
                args,
                destination,
                source,
                ..
            } => {
                self.current_source = source.clone();
                if let CallTarget::Indirect(callee) = func {
                    self.consume_operand(callee);
                }
                for arg in args {
                    self.consume_operand(arg);
                }
                self.write_destination(destination);
            }
            Terminator::MacroCall {
                args,
                destination,
                source,
                ..
            } => {
                self.current_source = source.clone();
                for arg in args {
                    self.consume_operand(arg);
                }
                self.write_destination(destination);
            }
            Terminator::Return(operand) => {
                self.current_source = None;
                self.consume_operand(operand);
                self.exit_states
                    .insert(self.active_block, self.current.clone());
            }
        }
    }

    /// Consumes an operand: reads its place, applying move/init checks.
    fn consume_operand(&mut self, operand: &Operand) {
        match operand {
            Operand::Constant(_, _) => {}
            Operand::Copy(place, source) => self.consume_copy_place(place, source.clone()),
            Operand::Move(place, source) => self.consume_move_place(place, source.clone()),
        }
    }

    /// A plain read of a place (no ownership transfer). Read validation always
    /// happens: even a `Copy` value can't be read before it is initialized or
    /// after it was moved out. Only the state transition differs from a full
    /// move.
    fn consume_copy_place(&mut self, place: &Place, source: Option<Source>) {
        if place_is_global(place) {
            for local in index_locals(place) {
                self.mark_read(local);
            }
            return;
        }
        for local in place_read_locals(place) {
            self.mark_read(local);
        }
        self.check_read(place, source.clone());
        for local in index_locals(place) {
            self.check_read(&Place::from_local(local), source.clone());
        }
    }

    /// A move of a place (ownership transfer).
    fn consume_move_place(&mut self, place: &Place, source: Option<Source>) {
        if place_is_global(place) {
            for local in index_locals(place) {
                self.mark_read(local);
            }
            return;
        }
        for local in place_read_locals(place) {
            self.mark_read(local);
        }

        // Moving a field out of a struct with an explicit `Drop` impl is forbidden
        // entirely.
        if first_field(place).is_some() && self.type_has_explicit_drop(place) {
            self.emit_drop_move_error(place, source.clone());
        }

        self.check_read(place, source.clone());
        for local in index_locals(place) {
            self.check_read(&Place::from_local(local), source.clone());
        }

        if !self.type_is_copy_place(place) {
            self.current.move_place(place);
        }
    }

    /// Writes into a place, supplying the struct field set when the
    /// destination is a field so reconstruction is tracked precisely.
    fn write_destination(&mut self, place: &Place) {
        if place_is_global(place) {
            return;
        }
        let is_field = matches!(place.projection.first(), Some(PlaceElem::Field(_)));
        if is_field && let Some(fields) = self.struct_fields_of(place.local) {
            self.current.write_struct_place(place, &fields);
            return;
        }
        self.current.write_place(place);
    }

    /// Field `DefId`s of the struct type of `local`, in declaration order.
    fn struct_fields_of(&self, local: LocalId) -> Option<Vec<DefId>> {
        let ty = self.snapshot.as_ref()?.locals.get(local.0 as usize)?.ty;
        match self.typecheck.interner.get(ty) {
            Type::Struct { def_id, .. } => self
                .typecheck
                .struct_info
                .get(def_id)
                .map(|info| info.fields.iter().map(|field| field.field_def).collect()),
            _ => None,
        }
    }

    /// Emits the appropriate diagnostic if `place` isn't safely readable.
    fn check_read(&mut self, place: &Place, source: Option<Source>) {
        let outcome = self.current.read_place(place);
        if let Some(error) = self.read_error(place, outcome, source) {
            self.diagnostics.push(error);
        }
    }

    fn read_error(
        &self,
        place: &Place,
        outcome: ReadOutcome,
        operand_source: Option<Source>,
    ) -> Option<FlowError> {
        let (name, _) = self.local_name_and_source(place.local)?;
        let src = operand_source.or_else(|| self.use_source(place.local))?;
        let span = src.span;

        match outcome {
            ReadOutcome::Ok => None,
            ReadOutcome::Uninitialized | ReadOutcome::MaybeUninitialized => {
                Some(FlowError::UseOfUninitialized {
                    name,
                    src: src.src(),
                    span,
                })
            }
            ReadOutcome::Moved | ReadOutcome::MaybeMoved => Some(FlowError::UseAfterMove {
                name,
                src: src.src(),
                span,
            }),
            ReadOutcome::PartiallyMoved => Some(FlowError::UseOfPartiallyMoved {
                name,
                src: src.src(),
                span,
            }),
        }
    }

    /// Best available source for a read of `local`: the current statement or
    /// terminator, falling back to the local's declaration.
    fn use_source(&self, local: LocalId) -> Option<zeen_ast::Source> {
        self.current_source.clone().or_else(|| {
            self.snapshot
                .as_ref()
                .and_then(|s| s.locals.get(local.0 as usize))
                .and_then(|info| info.source.clone())
        })
    }

    fn emit_drop_move_error(&mut self, place: &Place, source: Option<Source>) {
        let Some((name, _)) = self.local_name_and_source(place.local) else {
            return;
        };
        let Some(src) = source.or_else(|| self.use_source(place.local)) else {
            return;
        };
        self.diagnostics.push(FlowError::MoveOutOfDrop {
            name,
            src: src.src(),
            span: src.span,
        });
    }

    fn local_name_and_source(&self, local: LocalId) -> Option<(SmolStr, Option<Source>)> {
        let info = self.snapshot.as_ref()?.locals.get(local.0 as usize)?;
        Some((info.name.clone()?, info.source.clone()))
    }

    fn mark_read(&mut self, local: LocalId) {
        self.read_locals.insert(local);
    }

    /// Reports `UnusedVariable` warnings for never-read user locals.
    fn report_unused(&mut self) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        for (i, info) in snapshot.locals.iter().enumerate() {
            if info.kind != LocalKind::UserVariable {
                continue;
            }
            let Some(name) = &info.name else {
                continue;
            };
            if name.starts_with('_') {
                continue;
            }
            if self.read_locals.contains(&LocalId(i as u32)) {
                continue;
            }
            let Some(src) = info.source.clone() else {
                continue;
            };

            if self
                .diagnostics
                .iter()
                .any(|d| matches!(d, FlowError::UnusedVariable { name: n, .. } if n == name))
            {
                continue;
            }

            self.diagnostics.push(FlowError::UnusedVariable {
                name: name.clone(),
                src: src.src(),
                span: src.span,
            });
        }
    }

    /// Inserts `Drop` statements at the exit blocks, using the live-set computed
    /// for each specific exit. Values live on one early-return path are not
    /// dropped on a path where they were already moved out.
    fn insert_drops(&mut self, function_id: MirFunctionId) {
        let exit_states = std::mem::take(&mut self.exit_states);
        if exit_states.is_empty() {
            return;
        }

        let is_drop_impl = self
            .program
            .functions
            .get(&function_id)
            .map(|func| func.is_drop_impl)
            .unwrap_or(false);
        let self_param = is_drop_impl
            .then(|| {
                self.program
                    .functions
                    .get(&function_id)
                    .and_then(|func| func.params.first().map(|&local| Place::from_local(local)))
            })
            .flatten();

        // A `drop` implementation sorts out its own `self`; giving it an
        // automatic scope-exit drop would call itself recursively.
        let retain = |drops: &mut DropSet| {
            if let Some(self_param) = &self_param {
                drops.places.retain(|place| {
                    !(place.projection.is_empty() && place.local == self_param.local)
                });
            }
        };

        let mut any_inserted = false;
        for (block_id, state) in &exit_states {
            let mut drops = {
                let function = self.program.functions.get(&function_id).unwrap();
                drop::collect_scope_drops(function, state, &self.typecheck.interner, self.typecheck)
            };
            retain(&mut drops);
            if drops.places.is_empty() {
                continue;
            }

            let function = self.program.functions.get_mut(&function_id).unwrap();
            drop::insert_drops(function, *block_id, &drops);
            any_inserted = true;
        }

        if any_inserted {
            self.functions_with_drops.push(function_id);
        }
    }

    /// Inserts `Drop` statements right before the `StorageDead` of a local,
    /// so nested block scopes end their values exactly when the scope ends.
    ///
    /// Unlike `insert_drops`, a `MaybeInitialized` value at scope end is a hard
    /// error: it is impossible to tell whether it was initialized on runtime,
    /// so a drop cannot be emitted safely.
    #[allow(clippy::needless_collect)]
    fn insert_scope_drops(&mut self, function_id: MirFunctionId) {
        let storage_states = std::mem::take(&mut self.storage_states);
        if storage_states.is_empty() {
            return;
        }

        #[derive(Default)]
        struct Plan {
            insertions: Vec<(usize, DropSet)>,
            errors: Vec<FlowError>,
        }
        let mut per_block: HashMap<BlockId, Plan> = HashMap::new();

        {
            let Some(snapshot) = &self.snapshot else {
                return;
            };
            let function = self.program.functions.get(&function_id).unwrap();

            for ((block, stmt_index), state) in &storage_states {
                let Some(MirStatement::StorageDead(local)) = snapshot.blocks[block.0 as usize]
                    .statements
                    .get(*stmt_index)
                else {
                    continue;
                };
                let info = &snapshot.locals[local.0 as usize];
                if info.kind == LocalKind::Temporary
                    || !drop::type_needs_drop(&self.typecheck.interner, self.typecheck, info.ty)
                {
                    continue;
                }

                match state.state_of(*local) {
                    LocalState::Whole(ValueState::Initialized) | LocalState::PartiallyMoved(_) => {
                        let mut drops = DropSet::default();
                        drop::collect_local_drops(
                            function,
                            *local,
                            state,
                            &self.typecheck.interner,
                            self.typecheck,
                            &mut drops,
                        );
                        if !drops.places.is_empty() {
                            let insert_at = drop_insert_position(
                                &snapshot.blocks[block.0 as usize].statements,
                                *stmt_index,
                                *local,
                            );
                            per_block
                                .entry(*block)
                                .or_default()
                                .insertions
                                .push((insert_at, drops));
                        }
                    }
                    LocalState::Whole(ValueState::MaybeInitialized) => {
                        if let Some((name, Some(src))) = self.local_name_and_source(*local) {
                            per_block.entry(*block).or_default().errors.push(
                                FlowError::MaybeUninitializedDrop {
                                    name,
                                    src: src.src(),
                                    span: src.span,
                                },
                            );
                        }
                    }
                    LocalState::Whole(ValueState::Uninitialized)
                    | LocalState::Whole(ValueState::Moved)
                    | LocalState::Whole(ValueState::MaybeMoved) => {}
                }
            }
        }

        // Insert drops deepest/rightmost first so earlier index shifts don't
        // disturb the positions of drops that come after them.
        for (block, mut plan) in per_block {
            plan.insertions
                .sort_by_key(|(index, _)| std::cmp::Reverse(*index));
            for (index, drops) in plan.insertions {
                let function = self.program.functions.get_mut(&function_id).unwrap();
                drop::insert_scope_drop(function, block, index, &drops);
            }
            self.diagnostics.extend(plan.errors);
        }
    }

    fn type_is_copy_place(&mut self, place: &Place) -> bool {
        let Some(ty) = self.type_at_place(place) else {
            return false;
        };
        self.type_is_copy(ty)
    }

    /// Moves out of a field of a struct with an *explicit* `Drop` implementation
    /// are rejected: dropping the partial value would bypass the
    /// implementation's `drop`. Structs that merely *contain* drop values (and
    /// drop per-field) may be partially moved freely.
    fn type_has_explicit_drop(&self, place: &Place) -> bool {
        let Some(ty) = self.root_type(place) else {
            return false;
        };
        match self.typecheck.interner.get(ty) {
            Type::Struct { def_id, .. } => self
                .typecheck
                .struct_info
                .get(def_id)
                .map(|info| info.capabalities.has_explicit_drop)
                .unwrap_or(false),
            _ => false,
        }
    }

    fn type_at_place(&mut self, place: &Place) -> Option<TypeId> {
        let mut ty = self
            .snapshot
            .as_ref()?
            .locals
            .get(place.local.0 as usize)?
            .ty;
        let mut bindings: HashMap<DefId, TypeId> = HashMap::new();
        for elem in &place.projection {
            let PlaceElem::Field(field) = elem else {
                return None;
            };
            let Type::Struct {
                def_id,
                generic_args,
            } = self.typecheck.interner.get(ty).clone()
            else {
                return None;
            };
            let field_info = self
                .typecheck
                .struct_info
                .get(&def_id)
                .and_then(|info| info.fields.iter().find(|f| f.field_def == *field))?;

            let params = self
                .typecheck
                .struct_generics
                .get(&def_id)
                .cloned()
                .unwrap_or_default();
            let mut nested = bindings;
            for (param, arg) in params.iter().zip(generic_args.iter().copied()) {
                let resolved = match self.typecheck.interner.get(arg) {
                    Type::GenericParam(def) => nested.get(def).copied().unwrap_or(arg),
                    _ => arg,
                };
                nested.insert(*param, resolved);
            }
            ty = zeen_types::substitute_generics(
                &mut self.typecheck.interner,
                field_info.field_ty,
                &nested,
            );
            bindings = nested;
        }
        Some(ty)
    }

    fn root_type(&self, place: &Place) -> Option<TypeId> {
        self.snapshot
            .as_ref()?
            .locals
            .get(place.local.0 as usize)
            .map(|info| info.ty)
    }

    fn resolve_name(&self, name: Spur) -> SmolStr {
        self.rodeo.borrow().resolve(&name).into()
    }

    /// Mirrors `zeen_mir::lowering::mir_type_is_copy`: copy types never move.
    fn type_is_copy(&self, ty: TypeId) -> bool {
        match self.typecheck.interner.get(ty).clone() {
            Type::Builtin(_)
            | Type::IntLiteral
            | Type::FloatLiteral
            | Type::Enum { .. }
            | Type::Pointer { .. }
            | Type::ManyPointer { .. }
            | Type::Fn { .. }
            | Type::Void
            | Type::Never
            | Type::Error => true,
            Type::Struct { def_id, .. } => self
                .typecheck
                .struct_info
                .get(&def_id)
                .map(|info| info.capabalities.is_copy)
                .unwrap_or(false),
            // `Fn` closure values duplicate their inline env on copy;
            // `FnOnce` owns a non-Copy capture and is move-only.
            Type::FatFn { once, .. } => !once,
            Type::Array { element, .. } => self.type_is_copy(element),
            Type::Slice { .. } => true,
            _ => false,
        }
    }
}

/// Block -> successor edges of the function.
fn compute_successors(snapshot: &FunctionSnapshot) -> HashMap<BlockId, Vec<BlockId>> {
    let mut result = HashMap::new();
    for (i, block) in snapshot.blocks.iter().enumerate() {
        let id = BlockId(i as u32);
        result.insert(id, successor_blocks(&block.terminator));
    }
    result
}

fn successor_blocks(terminator: &Terminator) -> Vec<BlockId> {
    match terminator {
        Terminator::Goto(target) => vec![*target],
        Terminator::SwitchInt {
            targets, otherwise, ..
        } => {
            let mut out: Vec<BlockId> = targets.iter().map(|(_, block)| *block).collect();
            out.push(*otherwise);
            out
        }
        Terminator::Call {
            target: Some(t), ..
        }
        | Terminator::MacroCall {
            target: Some(t), ..
        } => vec![*t],
        _ => Vec::new(),
    }
}

/// First `Field` projection of a place, if any.
fn first_field(place: &Place) -> Option<DefId> {
    match place.projection.first() {
        Some(PlaceElem::Field(field)) => Some(*field),
        _ => None,
    }
}

/// Statement index at which a scope-end drop for `local` should be inserted:
/// right after the last statement that reads `local`, provided nothing rewrites
/// `local` between that read and the scope end. Otherwise it stays right before
/// the `StorageDead` at `storage_idx`.
fn drop_insert_position(statements: &[MirStatement], storage_idx: usize, local: LocalId) -> usize {
    let last_read = statements[..storage_idx]
        .iter()
        .rposition(|stmt| stmt_reads_local(stmt, local));

    match last_read {
        Some(use_idx)
            if !statements[use_idx + 1..storage_idx]
                .iter()
                .any(|stmt| stmt_writes_local(stmt, local)) =>
        {
            use_idx + 1
        }
        _ => storage_idx,
    }
}

/// Whether the statement reads (uses) `local`'s value as an operand.
fn stmt_reads_local(stmt: &MirStatement, local: LocalId) -> bool {
    match stmt {
        MirStatement::Assign { place, rvalue, .. } => {
            rvalue_reads_local(rvalue, local) || index_locals(place).contains(&local)
        }
        MirStatement::Discard(operand) => operand_reads_local(operand, local),
        _ => false,
    }
}

/// Whether the statement (re)writes `local`, which would make the value to drop
/// a different one than the value already planned for.
fn stmt_writes_local(stmt: &MirStatement, local: LocalId) -> bool {
    match stmt {
        MirStatement::Assign { place, .. } => place.local == local,
        _ => false,
    }
}

fn rvalue_reads_local(rvalue: &Rvalue, local: LocalId) -> bool {
    match rvalue {
        Rvalue::Use(operand) => operand_reads_local(operand, local),
        Rvalue::BinaryOp { lhs, rhs, .. } => {
            operand_reads_local(lhs, local) || operand_reads_local(rhs, local)
        }
        Rvalue::UnaryOp { operand, .. } => operand_reads_local(operand, local),
        Rvalue::Cast { operand, .. } => operand_reads_local(operand, local),
        Rvalue::Aggregate { operands, .. } => operands
            .iter()
            .any(|operand| operand_reads_local(operand, local)),
        Rvalue::Ref { place, .. } => place_read_locals(place).contains(&local),
        Rvalue::Discriminant(place) => place_read_locals(place).contains(&local),
        Rvalue::SizeOf(_) | Rvalue::AlignOf(_) => false,
    }
}

fn operand_reads_local(operand: &Operand, local: LocalId) -> bool {
    match operand {
        Operand::Copy(place, _) | Operand::Move(place, _) => {
            place_read_locals(place).contains(&local)
        }
        Operand::Constant(_, _) => false,
    }
}

/// Base local of a place plus every index local in its projection: reading
/// `arr.ptr[i]` reads both `arr` and `i`.
fn place_read_locals(place: &Place) -> Vec<LocalId> {
    let mut locals = vec![place.local];
    locals.extend(index_locals(place));
    locals
}

/// Index locals referenced by a place's projection.
fn index_locals(place: &Place) -> Vec<LocalId> {
    place
        .projection
        .iter()
        .filter_map(|elem| match elem {
            PlaceElem::Index(local) => Some(*local),
            _ => None,
        })
        .collect()
}

/// For each local that (transitively) holds a borrow, the set of frame locals
/// its slice/pointer points into. An empty set means the value owns no borrow
/// of this function's frame (external slice, string literal, plain data).
type BorrowState = HashMap<LocalId, HashSet<LocalId>>;

/// Transfer of a statement over the borrow state.
fn apply_borrow_stmt(state: &mut BorrowState, stmt: &MirStatement) {
    let MirStatement::Assign { place, rvalue, .. } = stmt else {
        return;
    };
    let dest_root = place.local;

    // Writes to a whole local replace its provenance; writes to a projected
    // field/array slot merge into it.
    let write = |state: &mut BorrowState, roots: HashSet<LocalId>| {
        if place.projection.is_empty() {
            state.insert(dest_root, roots);
        } else {
            state.entry(dest_root).or_default().extend(roots);
        }
    };

    match rvalue {
        Rvalue::Use(op) => match op {
            Operand::Copy(p, _) | Operand::Move(p, _) => {
                let roots = state.get(&p.local).cloned().unwrap_or_default();
                write(state, roots);
            }
            Operand::Constant(_, _) => write(state, HashSet::new()),
        },

        Rvalue::Ref {
            place: ref_place, ..
        } => {
            if place_is_global(ref_place) {
                write(state, HashSet::new());
            } else {
                write(state, HashSet::from([ref_place.local]));
            }
        }

        Rvalue::Aggregate { operands, .. } => {
            let mut roots = HashSet::new();
            for op in operands {
                if let Operand::Copy(p, _) | Operand::Move(p, _) = op {
                    roots.extend(state.get(&p.local).cloned().unwrap_or_default());
                }
            }
            write(state, roots);
        }

        // Binary ops, casts, discriminants and size queries produce scalars
        // that never borrow frame memory.
        Rvalue::BinaryOp { .. }
        | Rvalue::UnaryOp { .. }
        | Rvalue::Cast { .. }
        | Rvalue::Discriminant(_)
        | Rvalue::SizeOf(_)
        | Rvalue::AlignOf(_) => write(state, HashSet::new()),
    }
}

/// Returns the borrowed frame local (and diagnostic source) when the returned
/// operand's provenance reaches into the current function's stack.
fn check_return_borrow(state: &BorrowState, op: &Operand) -> Option<(LocalId, Option<Source>)> {
    match op {
        Operand::Copy(p, src) | Operand::Move(p, src) => state
            .get(&p.local)
            .and_then(|roots| roots.iter().next().copied().map(|root| (root, src.clone()))),
        Operand::Constant(_, _) => None,
    }
}

/// Unions `incoming` borrow origins into `target`. Returns true if anything
/// changed (so the worklist re-visits the successor). Conservative join: an
/// origin that was overwritten on one path stays, which can only over-approximate.
fn merge_borrow_states(target: &mut BorrowState, incoming: &BorrowState) -> bool {
    let mut changed = false;
    for (local, roots) in incoming {
        let entry = target.entry(*local).or_default();
        for root in roots {
            changed |= entry.insert(*root);
        }
    }
    changed
}

impl<'ctx> DataFlow<'ctx> {
    /// Reports values that escape a function while still borrowing its stack
    /// frame: returning `&local` (or a slice/struct holding such a borrow) is
    /// always a dangling pointer once the frame is popped.
    fn check_escaping_borrows(&mut self) {
        let function_ids: Vec<MirFunctionId> = self.program.functions.keys().copied().collect();

        for function_id in function_ids {
            let snapshot = {
                let function = self.program.functions.get(&function_id).unwrap();
                FunctionSnapshot {
                    entry_block: function.entry_block,
                    params: function.params.clone(),
                    locals: function
                        .locals
                        .iter()
                        .map(|decl| LocalInfo {
                            ty: decl.ty,
                            kind: decl.kind,
                            name: decl.name.map(|name| self.resolve_name(name)),
                            source: decl.source.clone(),
                        })
                        .collect(),
                    blocks: function.blocks.clone(),
                }
            };

            self.check_function_escaping_borrows(&snapshot);
        }
    }

    fn check_function_escaping_borrows(&mut self, snapshot: &FunctionSnapshot) {
        let successors = compute_successors(snapshot);
        let mut in_states: HashMap<BlockId, BorrowState> = HashMap::new();
        let mut worklist = vec![snapshot.entry_block];
        in_states.insert(snapshot.entry_block, BorrowState::new());

        while let Some(block_id) = worklist.pop() {
            let mut state = in_states.get(&block_id).cloned().unwrap_or_default();
            let block = &snapshot.blocks[block_id.0 as usize];

            for stmt in &block.statements {
                apply_borrow_stmt(&mut state, stmt);
            }

            match &block.terminator {
                Terminator::Return(op) => {
                    if let Some((root, src)) = check_return_borrow(&state, op) {
                        let info = &snapshot.locals[root.0 as usize];
                        let name = info.name.clone().unwrap_or_else(|| SmolStr::from("value"));
                        let (src, span) =
                            src.as_ref().map(|s| (s.src(), s.span)).unwrap_or_else(|| {
                                let source = info.source.as_ref().unwrap();
                                (source.src(), source.span)
                            });
                        self.diagnostics
                            .push(FlowError::EscapingBorrow { name, src, span });
                    }
                }
                // A call's result cannot borrow this frame (the callee cannot
                // produce a borrow of a local it never sees on that path).
                Terminator::Call { destination, .. } => {
                    state.insert(destination.local, HashSet::new());
                }
                Terminator::Goto(_)
                | Terminator::SwitchInt { .. }
                | Terminator::MacroCall { .. }
                | Terminator::Unreachable => {}
            }

            let Some(succ_ids) = successors.get(&block_id) else {
                continue;
            };
            for succ in succ_ids {
                match in_states.entry(*succ) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(state.clone());
                        worklist.push(*succ);
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        if merge_borrow_states(entry.get_mut(), &state) {
                            worklist.push(*succ);
                        }
                    }
                }
            }
        }
    }

    /// Finalizes the pass, splitting diagnostics into errors and warnings.
    /// Fails (returns `Err`) if any error-severity diagnostic was emitted.
    pub fn finish(self) -> Result<FlowResult, Vec<FlowError>> {
        let mut result = FlowResult::default();

        for diagnostic in self.diagnostics {
            if is_error(&diagnostic) {
                result.errors.push(diagnostic);
            } else {
                result.warnings.push(diagnostic);
            }
        }

        result.functions_with_drops = self.functions_with_drops;

        if result.errors.is_empty() {
            Ok(result)
        } else {
            Err(result.errors)
        }
    }
}

fn is_error(diagnostic: &FlowError) -> bool {
    !matches!(diagnostic.severity(), Some(Severity::Warning))
}
