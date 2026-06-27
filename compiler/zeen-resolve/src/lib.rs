#![allow(unused)]

use bumpalo::Bump;
use lasso::Rodeo;

use std::{
    cell::RefCell,
    path::Path,
    rc::Rc,
    sync::{Arc, Mutex},
};

use error::ResolveError;
use resolvers::{include_resolver, name_resolver};

use zeen_ast::Declaration;
use zeen_driver::CompilationContext;

pub use resolution::{DefId, DefInfo, DefKind, NodeKey, Resolution, ResolutionResult};

mod error;
mod resolution;
mod resolvers;
mod symbol_table;

pub fn resolve(
    filename: Rc<String>,
    src: Arc<String>,

    entry_path: &Path,
    entry_program: &[&Declaration<'_>],

    arena: &Bump,
    interner: Rc<RefCell<Rodeo>>,
    context: &mut CompilationContext,
) -> Result<ResolutionResult, Vec<ResolveError>> {
    let mut include_resolver = include_resolver::IncludeResolver::new(
        Rc::clone(&filename),
        Arc::clone(&src),
        arena,
        Rc::clone(&interner),
        context,
    );

    let resolved_program = include_resolver.resolve(
        entry_path.to_path_buf(),
        entry_program,
        miette::NamedSource::new(filename.as_str(), Arc::clone(&src)),
    )?;

    let mut name_resolver = name_resolver::NameResolver::new(filename, src, arena, interner);
    name_resolver.resolve_module(resolved_program);

    let resolution_result = name_resolver.finish()?;

    Ok(resolution_result)
}
