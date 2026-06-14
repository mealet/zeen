#![allow(unused)]

use bumpalo::Bump;
use lasso::Rodeo;

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use error::ResolveError;
use resolvers::{include_resolver, name_resolver};

use zeen_ast::Declaration;
use zeen_driver::CompilationContext;

mod error;
mod resolution;
mod resolvers;
mod symbol_table;

pub fn resolve(
    filename: Arc<String>,
    src: Arc<String>,

    entry_path: &Path,
    entry_program: &[&Declaration<'_>],

    arena: &Bump,
    interner: Arc<Mutex<Rodeo>>,
    context: &mut CompilationContext,
) -> Result<(), Vec<ResolveError>> {
    let mut include_resolver = include_resolver::IncludeResolver::new(
        Arc::clone(&filename),
        Arc::clone(&src),
        arena,
        Arc::clone(&interner),
        context,
    );

    let resolved_program = include_resolver.resolve(
        entry_path.to_path_buf(),
        entry_program,
        miette::NamedSource::new(filename.as_str(), Arc::clone(&src)),
    )?;

    let mut name_resolver = name_resolver::NameResolver::new(filename, src, arena, interner);
    name_resolver.resolve_module(resolved_program)?;

    Ok(())
}
