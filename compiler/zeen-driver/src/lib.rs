use std::{collections::HashSet, path::PathBuf};

mod target;

use miette::GraphicalTheme;
pub use target::Target;

include!(concat!(env!("OUT_DIR"), "/core_files.rs"));

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

    pub fn use_color(mut self, flag: bool) -> Self {
        self.reporter = if flag {
            self.reporter.with_theme(GraphicalTheme::default())
        } else {
            self.reporter.with_theme(GraphicalTheme::unicode_nocolor())
        };

        self
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
    pub target: Option<String>,
    pub warnings: Vec<String>,
}

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

impl std::fmt::Display for CompilationMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Debug => "debug",
                Self::Release => "release",
            }
        )
    }
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
    pub packages: Vec<(String, PathBuf)>,
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
