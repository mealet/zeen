use rustc_hash::FxHashSet as HashSet;
use std::path::{Path, PathBuf};

mod target;
mod warnings;

use miette::GraphicalTheme;
pub use target::Target;
pub use warnings::source_disables_warnings;

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

pub fn primitive_modules(std_root: Option<&Path>) -> Vec<String> {
    let Some(root) = std_root else {
        return Vec::new();
    };

    let Ok(entries) = std::fs::read_dir(root.join("primitives")) else {
        return Vec::new();
    };

    let mut modules: Vec<String> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();

            if !path.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("zn") {
                return None;
            }

            let stem = path.file_stem().and_then(|stem| stem.to_str())?;

            Some(format!("std.primitives.{stem}"))
        })
        .collect();

    modules.sort();
    modules
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

    #[test]
    fn primitive_modules_are_sorted_and_filtered() {
        let root =
            std::env::temp_dir().join(format!("zeen-driver-primitives-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("primitives").join("nested.zn")).unwrap();
        std::fs::write(root.join("primitives").join("u8.zn"), "").unwrap();
        std::fs::write(root.join("primitives").join("i8.zn"), "").unwrap();
        std::fs::write(root.join("primitives").join("notes.txt"), "").unwrap();

        assert_eq!(
            primitive_modules(Some(&root)),
            vec![
                "std.primitives.i8".to_string(),
                "std.primitives.u8".to_string()
            ]
        );
        assert!(primitive_modules(None).is_empty());
        assert!(primitive_modules(Some(&root.join("missing"))).is_empty());

        let _ = std::fs::remove_dir_all(&root);
    }
}
