use zeen_mir::MirFunctionId;

use crate::error::FlowError;

#[derive(Debug, Default)]
pub struct FlowResult {
    pub errors: Vec<FlowError>,
    pub warnings: Vec<FlowError>,
    pub functions_with_drops: Vec<MirFunctionId>,
}
