use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use bumpalo::Bump;
use lasso::{Rodeo, Spur};
use miette::{NamedSource, SourceSpan};

use crate::error::ResolveError;
use zeen_ast::declarations::{Declaration, DeclarationKind};

fn resolve_use_path(
    raw: &str,
    current_file: &Path,
    project_root: &Path,
    std_dir: Option<&Path>,

    src: NamedSource<Arc<String>>,
    span: SourceSpan,
) -> Result<PathBuf, ResolveError> {
    let segments: Vec<&str> = raw.split('.').collect();

    if segments.is_empty() {
        return Err(ResolveError::FileNotFound {
            path: raw.into(),
            src,
            span,
        });
    }

    let current_dir = current_file.parent().unwrap_or_else(|| Path::new("."));

    let (base_dir, rest): (PathBuf, &[&str]) = match segments[0] {
        "root" => (project_root.to_path_buf(), &segments[1..]),
        "super" => (
            current_dir.parent().unwrap_or(current_dir).to_path_buf(),
            &segments[1..],
        ),
        "std" => match std_dir {
            Some(dir) => (dir.to_path_buf(), &segments[1..]),
            None => return Err(ResolveError::StdlibNotConfigured { src, span }),
        },
        _ => (current_dir.to_path_buf(), &segments[..]),
    };

    let mut path = base_dir;

    for seg in rest {
        path.push(seg);
    }

    path.set_extension("zn");

    Ok(path)
}

fn decl_is_pub(decl: &Declaration) -> bool {
    match &decl.kind {
        DeclarationKind::FnDecl { is_pub, .. } => *is_pub,
        DeclarationKind::StructDecl { is_pub, .. } => *is_pub,
        DeclarationKind::InterfaceDecl { is_pub, .. } => *is_pub,
        DeclarationKind::EnumDecl { is_pub, .. } => *is_pub,

        DeclarationKind::ExternVar { .. }
        | DeclarationKind::ExternLink { .. }
        | DeclarationKind::ExternInclude { .. } => true,

        DeclarationKind::ImplementDecl { .. } => true,
        DeclarationKind::Use { .. } => false,
    }
}

fn canonicalize_best_effort(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
