#![allow(unused)]

mod span;

pub use span::LineOffsets;
pub use span::LocationSpan;

use std::{collections::HashSet, path::PathBuf};

pub struct MietteDriver {
    reporter: miette::GraphicalReportHandler,
}

impl Default for MietteDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl MietteDriver {
    pub fn new() -> Self {
        let reporter = miette::GraphicalReportHandler::new()
            .tab_width(2)
            .with_links(true)
            .with_cause_chain();

        Self { reporter }
    }

    pub fn report(&self, diagnostic: &dyn miette::Diagnostic) -> Result<String, std::fmt::Error> {
        let mut buffer = String::new();

        self.reporter.render_report(&mut buffer, diagnostic)?;

        Ok(buffer)
    }
}

pub struct CompilationContext {
    pub paths: PathsConfig,
    pub mode: CompilationMode,
    pub output: CompilationOutput,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum CompilationMode {
    #[default]
    Debug,
    Release,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum CompilationOutput {
    #[default]
    Binary,
    Object,
    EmitIR,
}

pub struct PathsConfig {
    pub project_root: PathBuf,
    pub std_root: Option<PathBuf>,
    pub linked: HashSet<PathBuf>,
}
