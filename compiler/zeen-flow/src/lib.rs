//! Dataflow analysis, move semantics and drop insertion pass.
//!
//! Runs right after MIR lowering and works on a mutable `MirProgram`,
//! tracking the state of every local/place at each point of the CFG:
//! `initialized` / `uninitialized` / `moved` / partially moved / maybe-* variants.
//!
//! On top of the analysis it performs drop insertion and reports move/init
//! errors plus unused-variable warnings (see `error` for diagnostics).

#![allow(unused)]

pub mod error;
pub mod result;

pub use error::FlowError;
pub use result::FlowResult;
