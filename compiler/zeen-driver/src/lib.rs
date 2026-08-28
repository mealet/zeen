use std::{collections::HashSet, path::PathBuf};

mod target;

pub use target::Target;

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
    pub core_files: Vec<(&'static str, &'static str)>,
    pub mode: CompilationMode,
    pub output: CompilationOutput,
    /// Target triple being compiled for (`None` = host).
    pub target: Option<String>,
}

/// Whether the compilation target requires a `main` entry point.
///
/// Every executable target does. Bare wasm (`wasm32-unknown-unknown`) does
/// not: the linker treats it as a module (`--no-entry`), so a program without
/// `main` is fine there.
pub fn target_requires_main(target: Option<&str>) -> bool {
    let Some(target) = target else {
        return true;
    };

    let target = Target::parse(target);

    !(target.arch.starts_with("wasm") && target.os == "unknown")
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum CompilationMode {
    #[value(name = "Debug")]
    #[default]
    Debug,

    #[value(name = "Release")]
    Release,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum CompilationOutput {
    #[value(name = "BIN")]
    #[default]
    Binary,
    #[value(name = "OBJ")]
    Object,
    #[value(name = "IR")]
    EmitIR,
    #[value(name = "MIR")]
    EmitMIR,
}

pub struct PathsConfig {
    pub project_root: PathBuf,
    pub std_root: Option<PathBuf>,
    pub linked: HashSet<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_required_for_executables() {
        assert!(target_requires_main(None));
        assert!(target_requires_main(Some("x86_64-unknown-linux-gnu")));
        assert!(target_requires_main(Some("x86_64-pc-windows-msvc")));
    }

    #[test]
    fn main_not_required_for_bare_wasm() {
        assert!(!target_requires_main(Some("wasm32-unknown-unknown")));
    }

    #[test]
    fn main_required_for_wasi() {
        assert!(target_requires_main(Some("wasm32-unknown-wasip1")));
    }
}
