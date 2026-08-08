use std::{cell::RefCell, rc::Rc};

use lasso::Rodeo;
use miette::{Diagnostic, Severity};
use zeen_mir::{BasicBlock, BlockId, MirFunctionId, MirProgram, MirStatement, Terminator};
use zeen_resolve::ResolutionResult;
use zeen_typecheck::result::TypeCheckResult;

use crate::{error::FlowError, result::FlowResult, state::FunctionState};

/// The dataflow pass over a lowered MIR program.
///
/// Consumes a mutable `MirProgram`, analyzes every function's CFG, reports
/// move/init diagnostics and inserts `Drop` statements where needed.
pub struct DataFlow<'ctx> {
    program: &'ctx mut MirProgram,
    typecheck: &'ctx TypeCheckResult,
    resolution: &'ctx ResolutionResult,
    rodeo: Rc<RefCell<Rodeo>>,

    /// In-progress state of the currently analyzed function.
    current: FunctionState,
    diagnostics: Vec<FlowError>,
    functions_with_drops: Vec<MirFunctionId>,
}

impl<'ctx> DataFlow<'ctx> {
    pub fn new(
        program: &'ctx mut MirProgram,
        typecheck: &'ctx TypeCheckResult,
        resolution: &'ctx ResolutionResult,
        rodeo: Rc<RefCell<Rodeo>>,
    ) -> Self {
        Self {
            program,
            typecheck,
            resolution,
            rodeo,
            current: FunctionState::default(),
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
    fn analyze_function(&mut self, _function_id: MirFunctionId) {
        // TODO: seed params, propagate state over the CFG with a worklist,
        // report diagnostics, then run drop insertion on this function.
        todo!("propagate state over blocks, report diagnostics, then insert drops")
    }

    /// Forward transfer over a single basic block.
    fn analyze_block(&mut self, _function_id: MirFunctionId, _block: &BasicBlock) {
        // TODO: apply each statement, then the terminator's effect
        // into the successor states.
        todo!("transfer state over statements and terminator")
    }

    /// Applies a statement's effect to `self.current`.
    fn apply_statement(&mut self, _stmt: &MirStatement) {
        // TODO: move/copy reads mark places moved/copied, writes reinitialize
        // them, StorageDead clears state if it happens.
        todo!("read/write/move state transitions for a statement")
    }

    /// Applies a terminator's effect to the successor states.
    fn apply_terminator(&mut self, _terminator: &Terminator, _from: &FunctionState) {
        // TODO: move args on calls, merge state into each successor block's
        // inbound state.
        todo!("transfer state to successor blocks")
    }

    /// Inserts `Drop` statements at scope exits for values live at the end.
    fn insert_drops(&mut self, _function_id: MirFunctionId) {
        // TODO: compute the exit-state, ask `drop::collect_scope_drops`, then
        // `drop::insert_drops`. Record the function in `functions_with_drops`.
        todo!("call drop::collect_scope_drops + drop::insert_drops")
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
