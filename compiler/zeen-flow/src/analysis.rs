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
    BasicBlock, BlockId, LocalId, LocalKind, MirFunctionId, MirProgram, MirStatement, Operand,
    Place, PlaceElem, Rvalue, Terminator,
};
use zeen_resolve::{DefId, ResolutionResult};
use zeen_typecheck::result::TypeCheckResult;
use zeen_types::{Type, TypeId};

use crate::{
    drop,
    error::FlowError,
    result::FlowResult,
    state::{FunctionState, ReadOutcome},
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
    typecheck: &'ctx TypeCheckResult,
    rodeo: Rc<RefCell<Rodeo>>,

    /// In-progress state of the currently analyzed function.
    current: FunctionState,
    /// Merged in-states of each block, used by the worklist.
    block_in_states: HashMap<BlockId, FunctionState>,
    /// States at the `Return` terminators, used for drop insertion.
    exit_states: Vec<FunctionState>,
    /// Locals read anywhere in the current function, for unused warnings.
    read_locals: HashSet<LocalId>,
    /// Cache of `field DefId -> field TypeId`, built from `struct_info`.
    field_types: HashMap<DefId, TypeId>,
    /// Snapshot of the function currently being analyzed.
    snapshot: Option<FunctionSnapshot>,
    /// Source of the statement/terminator currently being processed, used to
    /// point borrow/move diagnostics at the offending use site.
    current_source: Option<Source>,

    diagnostics: Vec<FlowError>,
    functions_with_drops: Vec<MirFunctionId>,
}

impl<'ctx> DataFlow<'ctx> {
    pub fn new(
        program: &'ctx mut MirProgram,
        typecheck: &'ctx TypeCheckResult,
        _resolution: &'ctx ResolutionResult,
        rodeo: Rc<RefCell<Rodeo>>,
    ) -> Self {
        let mut field_types = HashMap::new();
        for info in typecheck.struct_info.values() {
            for field in &info.fields {
                field_types.insert(field.field_def, field.field_ty);
            }
        }

        Self {
            program,
            typecheck,
            rodeo,
            current: FunctionState::default(),
            block_in_states: HashMap::new(),
            exit_states: Vec::new(),
            read_locals: HashSet::new(),
            field_types,
            snapshot: None,
            current_source: None,
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
    }

    /// Dataflow over a single function, ending with drop insertion
    /// into its exit blocks.
    fn analyze_function(&mut self, function_id: MirFunctionId) {
        self.current.clear();
        self.block_in_states.clear();
        self.exit_states.clear();
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

            let block = &blocks[block_id.0 as usize];
            self.current_source = None;
            for stmt in &block.statements {
                self.apply_statement(stmt);
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

        self.insert_drops(function_id);
    }

    /// Applies a statement's effect to `self.current`.
    fn apply_statement(&mut self, stmt: &MirStatement) {
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
                        self.consume_copy_place(ref_place);
                    }
                    Rvalue::Aggregate { operands, .. } => {
                        for operand in operands {
                            self.consume_operand(operand);
                        }
                    }
                    Rvalue::Discriminant(place) => self.consume_copy_place(place),
                    Rvalue::SizeOf(_) | Rvalue::AlignOf(_) => {}
                }

                self.current.write_place(place);
            }
            MirStatement::Drop(place) => {
                self.current_source = None;
                self.consume_move_place(place);
            }
            MirStatement::StorageLive(_) | MirStatement::StorageDead(_) | MirStatement::Nop => {
                self.current_source = None;
            }
        }
    }

    /// Applies a terminator's effect: consumes operands, writes the call
    /// destination, and records exit states on `Return`.
    fn apply_terminator(&mut self, terminator: &Terminator) {
        match terminator {
            Terminator::Goto(_) => {}
            Terminator::SwitchInt { discriminant, .. } => {
                self.consume_operand(discriminant);
            }
            Terminator::Call {
                args,
                destination,
                source,
                ..
            }
            | Terminator::MacroCall {
                args,
                destination,
                source,
                ..
            } => {
                self.current_source = source.clone();
                for arg in args {
                    self.consume_operand(arg);
                }
                self.current.write_place(destination);
            }
            Terminator::Return(operand) => {
                self.current_source = None;
                self.consume_operand(operand);
                self.exit_states.push(self.current.clone());
            }
            Terminator::Goto(_) => {
                self.current_source = None;
            }
            Terminator::SwitchInt { discriminant, .. } => {
                self.current_source = None;
                self.consume_operand(discriminant);
            }
            Terminator::Unreachable => {
                self.current_source = None;
            }
        }
    }

    /// Consumes an operand: reads its place, applying move/init checks.
    fn consume_operand(&mut self, operand: &Operand) {
        match operand {
            Operand::Constant(_) => {}
            Operand::Copy(place) => self.consume_copy_place(place),
            Operand::Move(place) => self.consume_move_place(place),
        }
    }

    /// A plain read of a place (no ownership transfer).
    fn consume_copy_place(&mut self, place: &Place) {
        self.mark_read(place.local);
        if self.type_is_copy_place(place) {
            return;
        }
        self.check_read(place);
    }

    /// A move of a place (ownership transfer).
    fn consume_move_place(&mut self, place: &Place) {
        self.mark_read(place.local);

        // Moving a field out of a `Drop` type is forbidden entirely.
        if first_field(place).is_some() && self.type_is_drop_place(place) {
            self.emit_drop_move_error(place);
        }

        if self.type_is_copy_place(place) {
            self.check_read(place);
            return;
        }

        self.check_read(place);
        self.current.move_place(place);
    }

    /// Emits the appropriate diagnostic if `place` isn't safely readable.
    fn check_read(&mut self, place: &Place) {
        let outcome = self.current.read_place(place);
        if let Some(error) = self.read_error(place, outcome) {
            self.diagnostics.push(error);
        }
    }

    fn read_error(&self, place: &Place, outcome: ReadOutcome) -> Option<FlowError> {
        let (name, _) = self.local_name_and_source(place.local)?;
        let src = self.use_source(place.local)?;
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

    fn emit_drop_move_error(&mut self, place: &Place) {
        let Some((name, _)) = self.local_name_and_source(place.local) else {
            return;
        };
        let Some(src) = self.use_source(place.local) else {
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

    /// Inserts `Drop` statements at scope exits for values live at the end.
    fn insert_drops(&mut self, function_id: MirFunctionId) {
        let exit_states = std::mem::take(&mut self.exit_states);
        if exit_states.is_empty() {
            return;
        }

        let mut combined = drop::DropSet::default();
        {
            let function = self.program.functions.get(&function_id).unwrap();
            for state in &exit_states {
                let drops = drop::collect_scope_drops(
                    function,
                    state,
                    &self.typecheck.interner,
                    self.typecheck,
                );
                combined.places.extend(drops.places);
            }
        }

        if combined.places.is_empty() {
            return;
        }

        let function = self.program.functions.get_mut(&function_id).unwrap();
        drop::insert_drops(function, &combined);
        self.functions_with_drops.push(function_id);
    }

    fn type_is_copy_place(&self, place: &Place) -> bool {
        let Some(ty) = self.type_at_place(place) else {
            return false;
        };
        self.type_is_copy(ty)
    }

    fn type_is_drop_place(&self, place: &Place) -> bool {
        let Some(ty) = self.root_type(place) else {
            return false;
        };
        drop::type_needs_drop(&self.typecheck.interner, self.typecheck, ty)
    }

    fn type_at_place(&self, place: &Place) -> Option<TypeId> {
        let mut ty = self
            .snapshot
            .as_ref()?
            .locals
            .get(place.local.0 as usize)?
            .ty;
        for elem in &place.projection {
            let PlaceElem::Field(field) = elem else {
                return None;
            };
            ty = *self.field_types.get(field)?;
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
            Type::Slice { .. } | Type::Array { .. } => false,
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

impl<'ctx> DataFlow<'ctx> {
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
