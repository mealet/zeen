use std::path::{Path, PathBuf};
use std::process::Command;

const REQUIRED_MAJOR: u32 = 22;
const PREFIX_ENV: &str = "LLVM_SYS_221_PREFIX";

const CONFIG_NAMES: [&str; 6] = [
    "llvm-config-22",
    "llvm-config22",
    "llvm22-config",
    "llvm-config-22.1",
    "llvm-config221",
    "llvm-config",
];

fn main() {
    println!("cargo:rerun-if-env-changed={PREFIX_ENV}");

    let prefix = std::env::var_os(PREFIX_ENV).map(PathBuf::from);

    for name in CONFIG_NAMES {
        let Some(binary) = locate(name, prefix.as_deref()) else {
            continue;
        };

        let Some(major) = major_version(&binary) else {
            continue;
        };

        if major != REQUIRED_MAJOR {
            panic!(
                "zeen-codegen-llvm requires LLVM {REQUIRED_MAJOR}, but {} reports LLVM {major} (point {PREFIX_ENV} at a LLVM {REQUIRED_MAJOR} prefix)",
                binary.display()
            );
        }

        return;
    }
}

fn locate(name: &str, prefix: Option<&Path>) -> Option<PathBuf> {
    match prefix {
        Some(root) => {
            let candidate = root.join("bin").join(name);
            candidate.is_file().then_some(candidate)
        }
        None => search_path(name),
    }
}

fn search_path(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;

    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);

        if candidate.is_file() {
            return Some(candidate);
        }

        if cfg!(windows) {
            let candidate = dir.join(format!("{name}.exe"));

            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

fn major_version(binary: &Path) -> Option<u32> {
    let output = Command::new(binary).arg("--version").output().ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8(output.stdout).ok()?;

    text.trim().split('.').next()?.parse().ok()
}
