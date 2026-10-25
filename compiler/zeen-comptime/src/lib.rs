mod capture;
mod check;
mod display;
mod error;
mod format;
mod interp;
mod subst;
mod value;

pub use error::Error;
pub use value::Value;

use bumpalo::Bump;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use lasso::Rodeo;

use zeen_ast::Declaration;

pub fn evaluate<'arena>(
    program: &'arena [&'arena Declaration<'arena>],
    arena: &'arena Bump,
    interner: Rc<RefCell<Rodeo>>,
    module_of: impl Fn(&Declaration<'_>) -> PathBuf,
    visibility: HashMap<PathBuf, Vec<PathBuf>>,
    target: &zeen_driver::Target,
) -> Result<&'arena [&'arena Declaration<'arena>], Vec<Error>> {
    let errors = check::validate_program(program, &interner);

    if !errors.is_empty() {
        return Err(errors);
    }

    let mut pass = interp::Interp::new(arena, interner, visibility, target);
    pass.evaluate(program, module_of)
}
