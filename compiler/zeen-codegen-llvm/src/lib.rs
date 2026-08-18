mod codegen;
mod error;

pub use codegen::{CodeGen, CodegenOptions};
pub use error::CodegenError;

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests;
