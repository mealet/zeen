//! Dataflow analysis, move semantics and drop insertion pass.
//!
//! Runs right after MIR lowering and works on a mutable `MirProgram`,
//! tracking the state of every local/place at each point of the CFG:
//! `initialized` / `uninitialized` / `moved` / partially moved / maybe-* variants.
//!
//! On top of the analysis it performs drop insertion and reports move/init
//! errors plus unused-variable warnings (see `error` for diagnostics).

use std::{cell::RefCell, rc::Rc};

use lasso::Rodeo;
use zeen_mir::MirProgram;
use zeen_resolve::ResolutionResult;
use zeen_typecheck::result::TypeCheckResult;

use crate::analysis::DataFlow;

pub mod analysis;
pub mod drop;
pub mod error;
pub mod result;
pub mod state;

pub use error::FlowError;
pub use result::FlowResult;

/// Runs the whole dataflow pass over a lowered MIR program.
///
/// This is the entry point for wiring the pass into the compiler pipeline
/// right after MIR lowering.
pub fn run_dataflow(
    program: &mut MirProgram,
    typecheck: &TypeCheckResult,
    resolution: &ResolutionResult,
    rodeo: Rc<RefCell<Rodeo>>,
) -> Result<FlowResult, Vec<FlowError>> {
    let mut flow = DataFlow::new(program, typecheck, resolution, rodeo);
    flow.run();

    flow.finish()
}
