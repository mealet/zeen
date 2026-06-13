use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use bumpalo::Bump;
use lasso::{Rodeo, Spur};
use miette::{NamedSource, SourceSpan};
use smol_str::SmolStr;

use crate::error::ResolveError;
use zeen_ast::declarations::{Declaration, DeclarationKind};

struct RawModule<'arena> {
    cannonical_path: PathBuf,
    decls: &'arena [&'arena Declaration<'arena>],
    named_src: NamedSource<Arc<String>>,
}

pub struct ImportResolver<'ctx> {
    arena: &'ctx Bump,
    interner: Arc<Mutex<Rodeo>>,
    context: zeen_driver::CompilationContext,

    modules: HashMap<PathBuf, RawModule<'ctx>>,

    src: Arc<String>,
    filename: &'ctx str,
    errors: Vec<ResolveError>,
}

impl<'ctx> ImportResolver<'ctx> {
    pub fn new(
        filename: &'ctx str,
        src: Arc<String>,

        arena: &'ctx Bump,
        interner: Arc<Mutex<Rodeo>>,
        context: zeen_driver::CompilationContext,
    ) -> Self {
        Self {
            arena,
            interner,
            context,

            src,
            filename,

            modules: HashMap::new(),
            errors: Vec::new(),
        }
    }

    fn interner_intern(&mut self, value: impl AsRef<str>) -> lasso::Spur {
        // compiler is not async/threaded (at least for now), so we're unwrapping lock
        let mut interner = self.interner.lock().unwrap();

        interner.get_or_intern(value)
    }

    fn interner_resolve(&self, key: &Spur) -> SmolStr {
        let interner = self.interner.lock().unwrap();
        let resolved = interner.resolve(key);

        resolved.into()
    }

    fn named_src(&self) -> NamedSource<Arc<String>> {
        let src_ref = Arc::clone(&self.src);

        miette::NamedSource::new(self.filename, src_ref)
    }

    pub fn resolve(
        &mut self,
        root_path: PathBuf,
        root_decls: &'ctx [&'ctx Declaration<'ctx>],
        root_named_src: NamedSource<Arc<String>>,
    ) -> Result<&'ctx [&'ctx Declaration<'ctx>], Vec<ResolveError>> {
        let root_cannonical = canonicalize_best_effort(&root_path);

        self.modules.insert(
            root_cannonical.clone(),
            RawModule {
                decls: root_decls,
                cannonical_path: root_cannonical.clone(),
                named_src: root_named_src,
            },
        );

        let mut visiting: HashSet<PathBuf> = HashSet::new();
        visiting.insert(root_cannonical.clone());

        todo!()
    }
}

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
