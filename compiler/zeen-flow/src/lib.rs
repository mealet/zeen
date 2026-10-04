use std::{cell::RefCell, rc::Rc};

use lasso::Rodeo;
use zeen_mir::MirProgram;
use zeen_resolve::ResolutionResult;
use zeen_typecheck::result::TypeCheckResult;

use crate::analysis::DataFlow;

pub mod analysis;
pub mod drop;
pub mod error;
pub mod liveness;
pub mod result;
pub mod state;

#[cfg(test)]
mod tests;

pub use error::FlowError;
pub use result::FlowResult;

pub fn run_dataflow(
    program: &mut MirProgram,
    typecheck: &mut TypeCheckResult,
    resolution: &ResolutionResult,
    rodeo: Rc<RefCell<Rodeo>>,
) -> Result<FlowResult, Vec<FlowError>> {
    let mut flow = DataFlow::new(program, typecheck, resolution, rodeo);
    flow.run();

    flow.finish()
}
