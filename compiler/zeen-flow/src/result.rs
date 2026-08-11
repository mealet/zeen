use zeen_mir::MirFunctionId;

use crate::error::FlowError;

/// Output of the dataflow pass.
#[derive(Debug, Default)]
pub struct FlowResult {
    pub errors: Vec<FlowError>,
    pub warnings: Vec<FlowError>,
    /// Functions where `Drop` statements were inserted by the pass.
    pub functions_with_drops: Vec<MirFunctionId>,
}
