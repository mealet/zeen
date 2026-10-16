use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tower_lsp_server::ls_types::{
    CodeAction, CodeActionKind, Position, Range, TextEdit, Uri, WorkspaceEdit,
};

use crate::position;
use crate::symbols::SymbolIndex;

pub fn use_path_for(
    current_dir: &Path,
    project_root: Option<&Path>,
    packages: &[(String, PathBuf)],
    target: &Path,
) -> Option<String> {
    let stem = target.file_stem().and_then(|stem| stem.to_str())?;

    let dotted = |dir: &Path| -> Option<String> {
        let relative = target.parent()?.strip_prefix(dir).ok()?;

        if relative.as_os_str().is_empty() {
            if stem == "index" {
                return Some(String::new());
            }

            return Some(stem.to_string());
        }

        let mut parts: Vec<String> = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect();

        if stem != "index" {
            parts.push(stem.to_string());
        }

        Some(parts.join("."))
    };

    if target.starts_with(current_dir) {
        if let Some(rest) = dotted(current_dir) {
            if rest.is_empty() {
                return Some(stem.to_string());
            }

            return Some(rest);
        }

        return None;
    }

    if let Some(parent) = current_dir.parent() {
        let dominated = project_root.is_some_and(|root| root == parent);

        if !dominated
            && target.starts_with(parent)
            && let Some(rest) = dotted(parent)
        {
            if rest.is_empty() {
                return Some("super".to_string());
            }

            return Some(format!("super.{rest}"));
        }
    }

    if let Some(root) = project_root
        && target.starts_with(root)
        && let Some(rest) = dotted(root)
    {
        if rest.is_empty() {
            return Some("root".to_string());
        }

        return Some(format!("root.{rest}"));
    }

    for (name, dir) in packages {
        if target.starts_with(dir)
            && let Some(rest) = dotted(dir)
        {
            if rest.is_empty() {
                return Some(name.clone());
            }

            return Some(format!("{name}.{rest}"));
        }
    }

    None
}

pub fn insert_after_uses(text: &str) -> u32 {
    let mut insert = 0;

    for (line, raw) in (0_u32..).zip(text.lines()) {
        let trimmed = raw.trim_start();

        if trimmed.starts_with("use ") || trimmed.starts_with("use\t") {
            insert = line + 1;
        }
    }

    insert
}

pub fn missing_use_actions(
    text: &str,
    uri: &Uri,
    index: &SymbolIndex,
    current_dir: &Path,
    project_root: Option<&Path>,
    packages: &[(String, std::path::PathBuf)],
    name: &str,
) -> Vec<CodeAction> {
    let mut paths: Vec<String> = Vec::new();

    for (path, file) in index.files() {
        for symbol in &file.symbols {
            if symbol.name != name || symbol.container.is_some() || !symbol.is_pub {
                continue;
            }

            if let Some(import) = use_path_for(current_dir, project_root, packages, path)
                && !paths.contains(&import)
            {
                paths.push(import);
            }
        }
    }

    paths.sort();

    let line = insert_after_uses(text);
    let at = position::position_to_offset(text, Position::new(line, 0));
    let range = position::span_to_range(text, at, 0);

    paths
        .into_iter()
        .map(|path| CodeAction {
            title: format!("Add use '{path}'"),
            kind: Some(CodeActionKind::QUICKFIX),
            diagnostics: None,
            edit: Some(WorkspaceEdit {
                changes: Some(HashMap::from([(
                    uri.clone(),
                    vec![TextEdit {
                        range,
                        new_text: format!("use {path};\n"),
                    }],
                )])),
                ..Default::default()
            }),
            ..Default::default()
        })
        .collect()
}

pub fn document_links(
    text: &str,
    open_dir: Option<&std::path::Path>,
    project_root: Option<&std::path::Path>,
    std_root: Option<&std::path::Path>,
    packages: &[(String, std::path::PathBuf)],
) -> Vec<((usize, usize), String, std::path::PathBuf)> {
    let Some(dir) = open_dir else {
        return Vec::new();
    };

    let Some(root) = project_root.or(Some(dir)) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let mut offset = 0;

    for line in text.lines() {
        let trimmed = line.trim_start();

        if let Some(rest) = trimmed
            .strip_prefix("use ")
            .or_else(|| trimmed.strip_prefix("use\t"))
        {
            let path = rest.split(';').next().unwrap_or("").trim();

            if !path.is_empty()
                && let Some(target) =
                    crate::modules::resolve_import_path(dir, root, std_root, packages, path)
            {
                let start = offset + line.find(path).unwrap_or(0);
                out.push(((start, start + path.len()), path.to_string(), target));
            }
        }

        offset += line.len() + 1;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_paths() {
        let root = PathBuf::from("/proj");
        let current = root.join("src");

        assert_eq!(
            use_path_for(&current, Some(&root), &[], &current.join("lib.zn")),
            Some("lib".to_string())
        );
        assert_eq!(
            use_path_for(
                &current,
                Some(&root),
                &[],
                &current.join("sub").join("lib.zn")
            ),
            Some("sub.lib".to_string())
        );
        assert_eq!(
            use_path_for(
                &current.join("sub"),
                Some(&root),
                &[],
                &current.join("lib.zn")
            ),
            Some("super.lib".to_string())
        );
        assert_eq!(
            use_path_for(&current, Some(&root), &[], &root.join("other.zn")),
            Some("root.other".to_string())
        );
        assert_eq!(
            use_path_for(
                &current,
                Some(&root),
                &[("http".to_string(), PathBuf::from("/deps/http"))],
                &PathBuf::from("/deps/http/client.zn")
            ),
            Some("http.client".to_string())
        );
        assert_eq!(
            use_path_for(
                &current,
                Some(&root),
                &[("http".to_string(), PathBuf::from("/deps/http"))],
                &PathBuf::from("/deps/http/index.zn")
            ),
            Some("http".to_string())
        );
        assert_eq!(
            use_path_for(
                &current,
                Some(&root),
                &[],
                &PathBuf::from("/elsewhere/a.zn")
            ),
            None
        );
    }

    #[test]
    fn inserts_after_last_use() {
        assert_eq!(insert_after_uses("fn main() {}\n"), 0);
        assert_eq!(insert_after_uses("use a;\nuse b;\nfn main() {}\n"), 2);
        assert_eq!(insert_after_uses("use a;\n"), 1);
    }

    #[test]
    fn links_point_at_resolved_files() {
        let dir = std::env::temp_dir().join(format!("zeen-lsp-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.zn"), "pub fn f() i32 {\n    return 1;\n}\n").unwrap();
        std::fs::write(dir.join("main.zn"), "use lib;\n\nfn main() {}\n").unwrap();

        let text = std::fs::read_to_string(dir.join("main.zn")).unwrap();
        let links = document_links(&text, Some(dir.as_path()), Some(dir.as_path()), None, &[]);

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].1, "lib".to_string());
        assert_eq!(links[0].2, dir.join("lib.zn"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
