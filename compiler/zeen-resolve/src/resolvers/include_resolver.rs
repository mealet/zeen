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
use zeen_ast::{
    Source,
    expressions::{Expression, ExpressionKind},
    statements::{Statement, StatementKind},
    types::{TypeExpr, TypeKind},
};
use zeen_driver::{CompilationMode, Target};

#[derive(Debug, Clone)]
struct RawModule<'arena> {
    canonical_path: PathBuf,
    decls: &'arena [&'arena Declaration<'arena>],
    named_src: NamedSource<Arc<String>>,
    is_core: bool,
}

/// Which std modules a program needs injected: `@format(...)` pulls in
/// `std.string`, closure/fat usage pulls in `std.fn`.
#[derive(Default)]
struct UsageFlags {
    has_format: bool,
    has_fat: bool,
}

pub struct IncludeResolver<'ctx> {
    arena: &'ctx Bump,
    interner: Rc<RefCell<Rodeo>>,
    context: &'ctx mut zeen_driver::CompilationContext,

    target: Target,
    mode: CompilationMode,

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
        target: Target,
        mode: CompilationMode,
    ) -> Self {
        Self {
            arena,
            interner,
            context,

            target,
            mode,

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

    /// Returns `true` if `raw` is a built-in module path (e.g. `std.alloc`)
    /// already injected into `self.modules`.
    fn is_builtin_module(&self, raw: &str) -> bool {
        self.modules.contains_key(Path::new(raw))
    }

    fn get_or_intern(&self, value: &str) -> Spur {
        self.interner.borrow_mut().get_or_intern(value)
    }

    /// Walks the whole AST for `@format` and closure/fat usage, deciding
    /// whether `std.string` / `std.fn` must be injected (both are only
    /// reachable from the filesystem std root, never embedded).
    fn usage_flags(&self, decls: &[&'ctx Declaration<'ctx>]) -> UsageFlags {
        let mut flags = UsageFlags::default();
        for decl in decls {
            self.decl_usage(decl, &mut flags);
        }
        flags
    }

    fn decl_usage(&self, decl: &Declaration<'ctx>, flags: &mut UsageFlags) {
        match &decl.kind {
            DeclarationKind::FnDecl {
                params,
                return_type,
                body,
                ..
            } => {
                for param in params.iter() {
                    self.type_usage(param.ty, flags);
                }
                if let Some(ret) = return_type {
                    self.type_usage(ret, flags);
                }
                if let Some(body) = body {
                    self.stmt_usage(body, flags);
                }
            }

            DeclarationKind::StructDecl {
                fields, methods, ..
            } => {
                for field in fields.iter() {
                    self.type_usage(field.ty, flags);
                }
                for method in methods.iter() {
                    self.decl_usage(method, flags);
                }
            }

            DeclarationKind::InterfaceDecl { methods, .. } => {
                for method in methods.iter() {
                    self.decl_usage(method, flags);
                }
            }

            DeclarationKind::ImplementDecl {
                object, methods, ..
            } => {
                for slot in object.2.iter() {
                    self.type_usage(slot, flags);
                }
                for method in methods.iter() {
                    self.decl_usage(method, flags);
                }
            }

            DeclarationKind::ExternVar { ty, .. } => self.type_usage(ty, flags),

            DeclarationKind::GlobalVar { ty, value, .. } => {
                self.type_usage(ty, flags);
                self.expr_usage(value, flags);
            }

            DeclarationKind::Alias(alias) => self.type_usage(alias.ty, flags),

            DeclarationKind::ConditionalBlock(block) => {
                for decl in block.body {
                    self.decl_usage(decl, flags);
                }
                if let Some(else_decl) = block.else_block {
                    self.decl_usage(else_decl, flags);
                }
            }

            _ => {}
        }
    }

    fn stmt_usage(&self, stmt: &Statement<'ctx>, flags: &mut UsageFlags) {
        match &stmt.kind {
            StatementKind::Let {
                explicit_type,
                value,
                ..
            } => {
                if let Some(ty) = explicit_type {
                    self.type_usage(ty, flags);
                }
                if let Some(value) = value {
                    self.expr_usage(value, flags);
                }
            }

            StatementKind::Assign { object, value }
            | StatementKind::CompoundAssign { object, value, .. } => {
                self.expr_usage(object, flags);
                self.expr_usage(value, flags);
            }

            StatementKind::Return { value } => {
                if let Some(value) = value {
                    self.expr_usage(value, flags);
                }
            }

            StatementKind::While { condition, block } => {
                self.expr_usage(condition, flags);
                self.stmt_usage(block, flags);
            }

            StatementKind::For {
                iterator, block, ..
            } => {
                self.expr_usage(iterator, flags);
                self.stmt_usage(block, flags);
            }

            StatementKind::FnDecl(decl) => self.decl_usage(decl, flags),

            StatementKind::Expr(expr) | StatementKind::TrailingExpr(expr) => {
                self.expr_usage(expr, flags)
            }

            StatementKind::ConditionalBlock(block) => {
                for stmt in block.stmts {
                    self.stmt_usage(stmt, flags);
                }
                if let Some(else_stmt) = block.else_block {
                    self.stmt_usage(else_stmt, flags);
                }
            }

            StatementKind::Break | StatementKind::Continue => {}
        }
    }

    fn expr_usage(&self, expr: &Expression<'ctx>, flags: &mut UsageFlags) {
        match &expr.kind {
            ExpressionKind::Literal(_)
            | ExpressionKind::Ident { .. }
            | ExpressionKind::TargetVar(_) => {}

            ExpressionKind::Binary { lhs, rhs, .. } => {
                self.expr_usage(lhs, flags);
                self.expr_usage(rhs, flags);
            }

            ExpressionKind::Unary { expr, .. } => self.expr_usage(expr, flags),

            ExpressionKind::Call { callee, args } => {
                self.expr_usage(callee, flags);
                for arg in args.iter() {
                    self.expr_usage(arg, flags);
                }
            }

            ExpressionKind::MacroCall { name, args } => {
                if self.interner_resolve(&name.0) == "format" {
                    flags.has_format = true;
                }
                for arg in args.iter() {
                    self.expr_usage(arg, flags);
                }
            }

            ExpressionKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.expr_usage(condition, flags);
                self.stmt_usage(then_block, flags);
                if let Some(else_block) = else_block {
                    self.stmt_usage(else_block, flags);
                }
            }

            ExpressionKind::Switch { object, arms } => {
                self.expr_usage(object, flags);
                for arm in arms.iter() {
                    self.expr_usage(arm.body, flags);
                    if let Some(guard) = arm.guard {
                        self.expr_usage(guard, flags);
                    }
                }
            }

            ExpressionKind::FieldAccess { object, field } => {
                self.expr_usage(object, flags);
                self.expr_usage(field, flags);
            }

            ExpressionKind::SliceAccess { object, index } => {
                self.expr_usage(object, flags);
                self.expr_usage(index, flags);
            }

            ExpressionKind::StructInit { ty, fields } => {
                self.expr_usage(ty, flags);
                if let Some(fields) = fields {
                    for field in fields.iter() {
                        self.expr_usage(field.value, flags);
                    }
                }
            }

            ExpressionKind::ArrayInit { elements } => {
                for element in elements.iter() {
                    self.expr_usage(element, flags);
                }
            }

            ExpressionKind::ArrayRepeatInit { element, len } => {
                self.expr_usage(element, flags);
                self.expr_usage(len, flags);
            }

            ExpressionKind::Block { stmts, trailing } => {
                for stmt in stmts.iter() {
                    self.stmt_usage(stmt, flags);
                }
                if let Some(expr) = trailing {
                    self.expr_usage(expr, flags);
                }
            }

            ExpressionKind::Type(ty) => self.type_usage(ty, flags),

            ExpressionKind::Closure {
                params,
                return_type,
                body,
            } => {
                flags.has_fat = true;
                for param in params.iter() {
                    self.type_usage(param.ty, flags);
                }
                if let Some(ret) = return_type {
                    self.type_usage(ret, flags);
                }
                self.stmt_usage(body, flags);
            }

            ExpressionKind::ConditionalBlock(block) => {
                self.expr_usage(block.body, flags);
                if let Some(else_expr) = block.else_block {
                    self.expr_usage(else_expr, flags);
                }
            }
        }
    }

    fn type_usage(&self, ty: &TypeExpr<'ctx>, flags: &mut UsageFlags) {
        match &ty.kind {
            TypeKind::FatFn { params, ret, .. } => {
                flags.has_fat = true;
                for param in params.iter() {
                    self.type_usage(param, flags);
                }
                self.type_usage(ret, flags);
            }

            TypeKind::Named {
                generic_args: Some(args),
                ..
            } => {
                for arg in args.iter() {
                    self.type_usage(arg, flags);
                }
            }

            TypeKind::Named {
                generic_args: None, ..
            } => {}

            TypeKind::Const(inner)
            | TypeKind::SinglePointer(inner)
            | TypeKind::ManyPointer(inner) => self.type_usage(inner, flags),

            TypeKind::TypeOf(expr) => self.expr_usage(expr, flags),

            TypeKind::Array { element, len } => {
                self.type_usage(element, flags);
                if let Some(len) = len {
                    self.expr_usage(len, flags);
                }
            }

            TypeKind::Fn { params, ret, .. } => {
                for param in params.iter() {
                    self.type_usage(param, flags);
                }
                self.type_usage(ret, flags);
            }

            _ => {}
        }
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
                is_core: false,
            },
        );

        let mut out: Vec<&'ctx Declaration<'ctx>> = Vec::new();

        for (name, content) in core_files {
            let source = Arc::new(content.to_string());
            let filename = Rc::new(name.to_string());

            let parsed_module = Self::parse_module(
                self.arena,
                &self.interner,
                &self.target,
                self.mode,
                Arc::clone(&source),
                filename,
            )?;
            parsed_module.iter().for_each(|decl| out.push(decl));

            self.modules.insert(
                Path::new(name).to_path_buf(),
                RawModule {
                    decls: parsed_module,
                    canonical_path: Path::new(name).to_path_buf(),
                    named_src: NamedSource::new(name, source),
                    is_core: true,
                },
            );
        }

        // std modules are never embedded; `@format` needs `std.string` and
        // closure/fat usage needs `std.fn`, so synthesize their `use` decls
        // for the resolver.
        let usage = self.usage_flags(root_decls);
        if usage.has_format || usage.has_fat {
            let span = SourceSpan::new(0.into(), 0);
            let source = root_decls
                .first()
                .map(|decl| decl.source.clone())
                .unwrap_or_else(|| Source::from((span, self.named_src())));

            if usage.has_format {
                self.push_synthetic_use(&mut out, "std.string", span, source.clone());
            }
            if usage.has_fat {
                self.push_synthetic_use(&mut out, "std.fn", span, source.clone());
            }
        }

        root_decls.iter().for_each(|decl| out.push(decl));

        let out_arena = self.arena.alloc_slice_copy(&out);

        self.check_collisions(out_arena);

        Ok(out_arena)
    }

    fn push_synthetic_use(
        &self,
        out: &mut Vec<&'ctx Declaration<'ctx>>,
        module: &str,
        span: SourceSpan,
        source: Source,
    ) {
        let use_std = self.arena.alloc(Declaration {
            kind: DeclarationKind::Use {
                module: (self.get_or_intern(module), span),
            },
            source,
        });

        out.push(use_std);
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
                is_core: false,
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

            if self.is_builtin_module(&raw) {
                continue;
            }

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

            // The std library has its own configured root dir, so only check
            // non-std uses. A use path that escapes the project root is
            // usually an accident, so flag it.
            if !raw.starts_with("std") {
                let project_root = canonicalize_best_effort(&self.context.paths.project_root);
                if !target_canonical.starts_with(&project_root) {
                    self.context.warnings.push(format!(
                        "use '{}' resolves outside the project root ({})",
                        raw,
                        target_canonical.display()
                    ));
                }
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

            let target_decls = match Self::parse_module(
                self.arena,
                &self.interner,
                &self.target,
                self.mode,
                source,
                Rc::new(target_name),
            ) {
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
                    is_core: false,
                },
            );

            visiting.insert(target_canonical.clone());
            self.load_links(&target_canonical, target_decls);
            self.load_uses(&target_canonical, target_decls, visiting);
            visiting.remove(&target_canonical);
        }
    }

    fn parse_module(
        arena: &'ctx Bump,
        interner: &Rc<RefCell<Rodeo>>,
        target: &Target,
        mode: CompilationMode,
        source: Arc<String>,
        filename: Rc<String>,
    ) -> Result<&'ctx [&'ctx Declaration<'ctx>], Vec<ResolveError>> {
        let mut tokens = zeen_lexer::tokenize(&source);
        let mut parser = zeen_parser::Parser::new(
            filename,
            Arc::clone(&source),
            &mut tokens,
            arena,
            Rc::clone(interner),
        );

        let program = parser.parse_program().map_err(|errors| {
            errors
                .iter()
                .map(|err| ResolveError::ModuleParseError(err.to_owned()))
                .collect::<Vec<ResolveError>>()
        })?;

        Ok(zeen_preprocessor::resolve(
            program, arena, interner, target, mode,
        ))
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

                    if self.is_builtin_module(&raw) {
                        continue;
                    }

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

    /// A bare `extern fn` (no body) is a declaration only: repeated
    /// declarations of the same symbol are harmless, like redeclaring a
    /// libc function in C when std modules are injected.
    fn is_bare_extern_fn(decl: &Declaration<'ctx>) -> bool {
        matches!(
            decl.kind,
            DeclarationKind::FnDecl {
                is_extern: true,
                body: None,
                ..
            }
        )
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
                DeclarationKind::ExternVar { name, is_pub, .. } => {
                    (NamespaceTag::Value, name.0, name.1, is_pub)
                }
                DeclarationKind::GlobalVar { name, is_pub, .. } => {
                    (NamespaceTag::Value, name.0, name.1, is_pub)
                }
                _ => continue,
            };

            let (ns, name, span, _) = entry;

            if let Some((first_span, first_decl)) = seen.get(&(ns, name)) {
                if Self::is_bare_extern_fn(first_decl) && Self::is_bare_extern_fn(decl) {
                    continue;
                }

                let name = self.interner_resolve(&entry.1);

                let first_definition = {
                    let (first_is_core, _, named_src) = self.module_source_of(first_decl);

                    if first_is_core {
                        let (_, _, redefinition_src) = self.module_source_of(decl);

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

                let (_, _, second_src) = self.module_source_of(decl);
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
    ) -> (bool, PathBuf, NamedSource<Arc<String>>) {
        let target_ptr = decl as *const Declaration as usize;

        for module in self.modules.values() {
            for d in module.decls {
                if (*d as *const Declaration as usize) == target_ptr {
                    return (
                        module.is_core,
                        module.canonical_path.clone(),
                        module.named_src.clone(),
                    );
                }
            }
        }

        (false, PathBuf::new(), self.named_src())
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
