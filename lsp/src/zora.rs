use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const MANIFEST: &str = "Zora.toml";
pub const LOCKFILE: &str = "Zora.lock";
pub const WALK_UP_LIMIT: usize = 8;

pub fn find_root(open_path: &Path) -> Option<PathBuf> {
    let mut cursor = open_path.parent().map(Path::to_path_buf)?;

    for _ in 0..WALK_UP_LIMIT {
        if cursor.join(MANIFEST).is_file() {
            return Some(cursor);
        }

        cursor = cursor.parent().map(Path::to_path_buf)?;
    }

    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Main,
    Index,
}

pub fn entry_for_root(root: &Path) -> Option<PathBuf> {
    let manifest = std::fs::read_to_string(root.join(MANIFEST)).ok()?;
    let kind = entry_kind(&manifest).unwrap_or(EntryKind::Main);

    Some(match kind {
        EntryKind::Main => root.join("src").join("main.zn"),
        EntryKind::Index => root.join("src").join("index.zn"),
    })
}

pub fn self_mapping(root: &Path) -> Option<(String, PathBuf)> {
    let manifest = std::fs::read_to_string(root.join(MANIFEST)).ok()?;
    let name = package_name(&manifest)?;
    let src = root.join("src");

    if src.is_dir() {
        Some((name, src))
    } else {
        None
    }
}

fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;

    for line in manifest.lines() {
        let line = line.trim();

        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }

        if !in_package {
            continue;
        }

        let Some(rest) = line.strip_prefix("name") else {
            continue;
        };

        if !rest.starts_with([' ', '\t', '=']) {
            continue;
        }

        let value = rest
            .trim_start_matches([' ', '\t', '='])
            .trim()
            .trim_matches('"')
            .trim();

        if value.is_empty() {
            continue;
        }

        return Some(value.to_string());
    }

    None
}

pub(crate) fn entry_kind(manifest: &str) -> Option<EntryKind> {
    let mut in_package = false;

    for line in manifest.lines() {
        let line = line.trim();

        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }

        if !in_package {
            continue;
        }

        let Some(rest) = line.strip_prefix("type") else {
            continue;
        };

        let value = rest
            .trim_start_matches([' ', '\t', '='])
            .trim()
            .trim_matches('"')
            .trim();

        match value {
            "bin" => return Some(EntryKind::Main),
            "lib/static" | "lib/dynamic" => return Some(EntryKind::Index),
            _ => continue,
        }
    }

    None
}

pub fn parse_deps_map(stdout: &str) -> Vec<(String, PathBuf)> {
    let mut pairs = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();

        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((name, path)) = line.split_once('=') else {
            continue;
        };

        let name = name.trim();
        let path = path.trim();

        if name.is_empty() || path.is_empty() {
            continue;
        }

        pairs.push((name.to_string(), PathBuf::from(path)));
    }

    pairs
}

pub struct DepsMap {
    pub packages: Vec<(String, PathBuf)>,
    pub stderr: Option<String>,
}

pub fn run_deps_map(root: &Path) -> DepsMap {
    let result = std::process::Command::new("zora")
        .arg("deps-map")
        .current_dir(root)
        .output();

    match result {
        Err(_) => DepsMap {
            packages: Vec::new(),
            stderr: None,
        },
        Ok(output) if output.status.success() => DepsMap {
            packages: parse_deps_map(&String::from_utf8_lossy(&output.stdout)),
            stderr: None,
        },
        Ok(output) => {
            let message = String::from_utf8_lossy(&output.stderr).trim().to_string();

            DepsMap {
                packages: Vec::new(),
                stderr: (!message.is_empty()).then_some(message),
            }
        }
    }
}

pub fn manifest_mtimes(root: &Path) -> (Option<SystemTime>, Option<SystemTime>) {
    let mtime = |name: &str| {
        std::fs::metadata(root.join(name))
            .and_then(|metadata| metadata.modified())
            .ok()
    };

    (mtime(MANIFEST), mtime(LOCKFILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(tag: &str, manifest: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("zeen-lsp-zora-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a").join("b").join("c")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join(MANIFEST), manifest).unwrap();
        dir
    }

    const BIN_MANIFEST: &str =
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nzeen = \"0.3\"\ntype = \"bin\"\n";
    const LIB_MANIFEST: &str =
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nzeen = \"0.3\"\ntype = \"lib/static\"\n";

    #[test]
    fn finds_root_deep_inside() {
        let dir = project("deep", BIN_MANIFEST);
        let deep = dir.join("a").join("b").join("c").join("main.zn");

        assert_eq!(find_root(&deep), Some(dir.clone()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stops_without_manifest() {
        let dir = std::env::temp_dir().join(format!("zeen-lsp-zora-plain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        let file = dir.join("a").join("main.zn");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        assert_eq!(find_root(&file), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn entry_follows_package_type() {
        let bin = project("bin", BIN_MANIFEST);
        let lib = project("lib", LIB_MANIFEST);

        assert_eq!(entry_for_root(&bin), Some(bin.join("src").join("main.zn")));
        assert_eq!(entry_for_root(&lib), Some(lib.join("src").join("index.zn")));

        let _ = std::fs::remove_dir_all(&bin);
        let _ = std::fs::remove_dir_all(&lib);
    }

    #[test]
    fn entry_defaults_to_main() {
        let dir = project(
            "notype",
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nzeen = \"0.3\"\n",
        );

        assert_eq!(entry_for_root(&dir), Some(dir.join("src").join("main.zn")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn self_mapping_reads_name() {
        let dir = project("selfmap", BIN_MANIFEST);

        assert_eq!(
            self_mapping(&dir),
            Some(("demo".to_string(), dir.join("src")))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn self_mapping_needs_src_dir() {
        let dir = std::env::temp_dir().join(format!("zeen-lsp-zora-nosrc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST), BIN_MANIFEST).unwrap();

        assert_eq!(self_mapping(&dir), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_deps_map_lines() {
        let pairs = parse_deps_map(
            "http=/tmp/dep/http/src\n\n# comment\nbadline\n=empty\nfoo= /tmp/dep/foo/src \n",
        );

        assert_eq!(
            pairs,
            vec![
                ("http".to_string(), PathBuf::from("/tmp/dep/http/src")),
                ("foo".to_string(), PathBuf::from("/tmp/dep/foo/src")),
            ]
        );
        assert!(parse_deps_map("").is_empty());
    }
}
