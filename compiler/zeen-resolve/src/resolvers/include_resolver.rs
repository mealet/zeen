use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

use bumpalo::Bump;
use lasso::{Rodeo, Spur};
use miette::{NamedSource, SourceSpan};
use smol_str::SmolStr;

use crate::error::ResolveError;
use zeen_ast::declarations::{Declaration, DeclarationKind};

#[derive(Debug, Clone)]
struct RawModule<'arena> {
    canonical_path: PathBuf,
    decls: &'arena [&'arena Declaration<'arena>],
    named_src: NamedSource<Arc<String>>,
}

pub struct IncludeResolver<'ctx> {
    arena: &'ctx Bump,
    interner: Rc<RefCell<Rodeo>>,
    context: &'ctx mut zeen_driver::CompilationContext,

    modules: HashMap<PathBuf, RawModule<'ctx>>,

    src: Arc<String>,
    filename: Rc<String>,
    errors: Vec<ResolveError>,
}

impl<'ctx> IncludeResolver<'ctx> {
    pub fn new(
        filename: Rc<String>,
        src: Arc<String>,

        arena: &'ctx Bump,
        interner: Rc<RefCell<Rodeo>>,
        context: &'ctx mut zeen_driver::CompilationContext,
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

    fn interner_resolve(&self, key: &Spur) -> SmolStr {
        let interner = self.interner.borrow();
        let resolved = interner.resolve(key);
        resolved.into()
    }

    fn named_src(&self) -> NamedSource<Arc<String>> {
        let src_ref = Arc::clone(&self.src);

        miette::NamedSource::new(self.filename.as_str(), src_ref)
    }

    pub fn resolve_core_injects(
        &mut self,
        root_path: PathBuf,
        root_decls: &'ctx [&'ctx Declaration<'ctx>],
        root_named_src: NamedSource<Arc<String>>,
        core_files: &[(&'static str, &'static str)],
    ) -> Result<&'ctx [&'ctx Declaration<'ctx>], Vec<ResolveError>> {
        let root_canonical = canonicalize_best_effort(&root_path);

        self.modules.insert(
            root_canonical.clone(),
            RawModule {
                decls: root_decls,
                canonical_path: root_canonical.clone(),
                named_src: root_named_src,
            },
        );

        let mut out: Vec<&'ctx Declaration<'ctx>> = Vec::new();

        for (name, content) in core_files {
            let source = Arc::new(content.to_string());
            let filename = Rc::new(name.to_string());

            let parsed_module = self.parse_module(Arc::clone(&source), filename)?;
            parsed_module.iter().for_each(|decl| out.push(decl));

            self.modules.insert(
                Path::new(name).to_path_buf(),
                RawModule {
                    decls: parsed_module,
                    canonical_path: Path::new(name).to_path_buf(),
                    named_src: NamedSource::new(name, source),
                },
            );
        }

        root_decls.iter().for_each(|decl| out.push(decl));

        let out_arena = self.arena.alloc_slice_copy(&out);

        self.check_collisions(out_arena);

        Ok(out_arena)
    }

    pub fn resolve(
        &mut self,
        root_path: PathBuf,
        root_decls: &'ctx [&'ctx Declaration<'ctx>],
        root_named_src: NamedSource<Arc<String>>,
    ) -> Result<&'ctx [&'ctx Declaration<'ctx>], &[ResolveError]> {
        let root_canonical = canonicalize_best_effort(&root_path);

        self.modules.insert(
            root_canonical.clone(),
            RawModule {
                decls: root_decls,
                canonical_path: root_canonical.clone(),
                named_src: root_named_src,
            },
        );

        let mut visiting: HashSet<PathBuf> = HashSet::new();
        visiting.insert(root_canonical.clone());
        self.load_links(&root_canonical, root_decls);
        self.load_uses(&root_canonical, root_decls, &mut visiting);

        if !self.errors.is_empty() {
            return Err(&self.errors);
        }

        let mut merged: Vec<&'ctx Declaration<'ctx>> = Vec::new();
        let mut visited_merge: HashSet<PathBuf> = HashSet::new();

        self.merge_module(&root_canonical, true, &mut merged, &mut visited_merge);

        if !self.errors.is_empty() {
            return Err(&self.errors);
        }

        self.check_collisions(&merged);

        if !self.errors.is_empty() {
            return Err(&self.errors);
        }

        Ok(self.arena.alloc_slice_copy(&merged))
    }

    fn load_links(&mut self, current_canonical: &Path, decls: &'ctx [&'ctx Declaration<'ctx>]) {
        for decl in decls {
            let DeclarationKind::ExternLink { path } = decl.kind else {
                continue;
            };

            let raw = self.interner_resolve(&path);
            let joined = current_canonical
                .parent()
                .unwrap_or(Path::new("."))
                .join(&raw);

            let target = canonicalize_best_effort(&joined);

            if !target.exists() {
                self.errors.push(ResolveError::LinkError {
                    message: "file doesn't exists".into(),
                    src: decl.source.src.clone(),
                    span: decl.source.span,
                });
                continue;
            }

            if !target.is_file() {
                self.errors.push(ResolveError::LinkError {
                    message: "path is a directory".into(),
                    src: decl.source.src.clone(),
                    span: decl.source.span,
                });
                continue;
            }

            if let Some(ext) = target.extension()
                && ext == "c"
            {
            } else {
                self.errors.push(ResolveError::LinkError {
                    message: "file extension must be `.c`".into(),
                    src: decl.source.src.clone(),
                    span: decl.source.span,
                });
                continue;
            }

            let _ = self.context.paths.linked.insert(target);
        }
    }

    fn load_uses(
        &mut self,
        current_canonical: &Path,
        decls: &'ctx [&'ctx Declaration<'ctx>],
        visiting: &mut HashSet<PathBuf>,
    ) {
        for decl in decls {
            let DeclarationKind::Use { module } = &decl.kind else {
                continue;
            };

            let raw = self.interner_resolve(&module.0);
            let target = match resolve_use_path(
                &raw,
                current_canonical,
                self.named_src(),
                &self.context.paths.project_root,
                self.context.paths.std_root.as_deref(),
                module.1,
            ) {
                Ok(pb) => pb,
                Err(err) => {
                    self.errors.push(*err);
                    continue;
                }
            };

            let target_canonical = canonicalize_best_effort(&target);

            if self.modules.contains_key(&target_canonical) || visiting.contains(&target_canonical)
            {
                continue;
            }

            let source = Arc::new(match fs::read_to_string(&target) {
                Ok(content) => content,
                Err(err) => {
                    self.errors.push(ResolveError::IoError {
                        message: err.to_string().into(),
                        src: self.named_src(),
                        span: module.1,
                    });
                    continue;
                }
            });

            let target_name = target_canonical
                .file_name()
                .unwrap_or(std::ffi::OsStr::new("unknown"))
                .to_string_lossy()
                .to_string();

            let named_src = NamedSource::new(&target_name, Arc::clone(&source));

            let target_decls = match self.parse_module(source, Rc::new(target_name)) {
                Ok(program) => program,
                Err(mut err) => {
                    self.errors.append(&mut err);
                    continue;
                }
            };

            self.modules.insert(
                target_canonical.clone(),
                RawModule {
                    named_src,
                    canonical_path: target_canonical.clone(),
                    decls: target_decls,
                },
            );

            visiting.insert(target_canonical.clone());
            self.load_links(&target_canonical, target_decls);
            self.load_uses(&target_canonical, target_decls, visiting);
            visiting.remove(&target_canonical);
        }
    }

    fn parse_module(
        &self,
        source: Arc<String>,
        filename: Rc<String>,
    ) -> Result<&'ctx [&'ctx Declaration<'ctx>], Vec<ResolveError>> {
        let mut tokens = zeen_lexer::tokenize(&source);
        let mut parser = zeen_parser::Parser::new(
            filename,
            Arc::clone(&source),
            &mut tokens,
            self.arena,
            Rc::clone(&self.interner),
        );

        let program = parser.parse_program().map_err(|errors| {
            errors
                .iter()
                .map(|err| ResolveError::ModuleParseError(err.to_owned()))
                .collect::<Vec<ResolveError>>()
        })?;

        Ok(program)
    }

    fn merge_module(
        &mut self,
        canonical: &Path,
        _is_root: bool,
        out: &mut Vec<&'ctx Declaration<'ctx>>,
        visited: &mut HashSet<PathBuf>,
    ) {
        if !visited.insert(canonical.to_path_buf()) {
            return;
        }

        let md = self.modules[canonical].clone();
        let decls = md.decls;

        for decl in decls {
            match decl.kind {
                DeclarationKind::Use { module } => {
                    let raw = self.interner_resolve(&module.0);

                    let Ok(target) = resolve_use_path(
                        &raw,
                        canonical,
                        md.named_src.clone(),
                        &self.context.paths.project_root,
                        self.context.paths.std_root.as_deref(),
                        module.1,
                    ) else {
                        continue;
                    };

                    let target_canonical = canonicalize_best_effort(&target);
                    self.merge_module(&target_canonical, false, out, visited);
                }

                _ => {
                    out.push(decl);
                }
            }
        }
    }

    fn check_collisions(&mut self, merged: &[&'ctx Declaration<'ctx>]) {
        #[derive(Eq, Hash, PartialEq, Clone, Copy)]
        enum NamespaceTag {
            Value,
            Type,
        }

        let mut seen: HashMap<(NamespaceTag, Spur), (SourceSpan, &'ctx Declaration<'ctx>)> =
            HashMap::new();

        for decl in merged {
            let entry: (NamespaceTag, Spur, SourceSpan, bool) = match decl.kind {
                DeclarationKind::FnDecl { name, is_pub, .. } => {
                    (NamespaceTag::Value, name.0, name.1, is_pub)
                }
                DeclarationKind::StructDecl { name, is_pub, .. } => {
                    (NamespaceTag::Type, name.0, name.1, is_pub)
                }
                DeclarationKind::InterfaceDecl { name, is_pub, .. } => {
                    (NamespaceTag::Type, name.0, name.1, is_pub)
                }
                DeclarationKind::EnumDecl { name, is_pub, .. } => {
                    (NamespaceTag::Value, name.0, name.1, is_pub)
                }
                DeclarationKind::ExternVar { name, .. } => {
                    (NamespaceTag::Value, name.0, name.1, false)
                }
                _ => continue,
            };

            let (ns, name, span, _) = entry;

            if let Some((first_span, first_decl)) = seen.get(&(ns, name)) {
                let name = self.interner_resolve(&entry.1);

                let first_definition = {
                    let (path, named_src) = self.module_source_of(first_decl);

                    if path.to_string_lossy().starts_with("core") {
                        let (_, redefinition_src) = self.module_source_of(decl);

                        self.errors.push(ResolveError::CoreReserved {
                            name,
                            src: redefinition_src,
                            span,
                        });

                        continue;
                    }

                    let content = Arc::unwrap_or_clone(named_src.inner().clone());
                    let filename = named_src.name().to_string();

                    crate::error::DuplicateLocation {
                        src: NamedSource::new(filename, content),
                        span: *first_span,
                    }
                };

                let (_, second_src) = self.module_source_of(decl);
                let content = Arc::unwrap_or_clone(second_src.inner().clone());
                let filename = second_src.name().to_string();

                let second_definition = crate::error::DuplicateLocation {
                    src: NamedSource::new(filename, content),
                    span,
                };

                self.errors.push(ResolveError::DuplicateDefinition {
                    name,
                    related: vec![first_definition, second_definition],
                });
            } else {
                seen.insert((ns, name), (span, decl));
            }
        }
    }

    fn module_source_of(
        &self,
        decl: &'ctx Declaration<'ctx>,
    ) -> (PathBuf, NamedSource<Arc<String>>) {
        let target_ptr = decl as *const Declaration as usize;

        for module in self.modules.values() {
            for d in module.decls {
                if (*d as *const Declaration as usize) == target_ptr {
                    return (module.canonical_path.clone(), module.named_src.clone());
                }
            }
        }

        (PathBuf::new(), self.named_src())
    }
}

fn resolve_use_path(
    raw: &str,
    current_file: &Path,
    current_src: NamedSource<Arc<String>>,

    project_root: &Path,
    std_dir: Option<&Path>,

    span: SourceSpan,
) -> Result<PathBuf, Box<ResolveError>> {
    let segments: Vec<&str> = raw.split('.').collect();

    if segments.is_empty() {
        return Err(Box::new(ResolveError::FileNotFound {
            path: raw.into(),
            src: current_src,
            span,
        }));
    }

    let current_dir = current_file.parent().unwrap_or_else(|| Path::new("."));

    let (base_dir, rest): (PathBuf, &[&str]) = match segments[0] {
        "root" => (project_root.to_path_buf(), &segments[1..]),
        "super" => (
            current_dir.parent().unwrap_or(current_dir).to_path_buf(),
            &segments[1..],
        ),
        "std" => match std_dir {
            Some(dir) => (canonicalize_best_effort(dir), &segments[1..]),
            None => {
                return Err(Box::new(ResolveError::StdlibNotConfigured {
                    src: current_src,
                    span,
                }));
            }
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

fn canonicalize_best_effort(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
