mod error;
mod interp;
mod subst;
mod value;

pub use error::Error;
pub use value::Value;

use bumpalo::Bump;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use lasso::Rodeo;

use zeen_ast::Declaration;

pub fn evaluate<'arena>(
    program: &'arena [&'arena Declaration<'arena>],
    arena: &'arena Bump,
    interner: Rc<RefCell<Rodeo>>,
    module_of: impl Fn(&Declaration<'_>) -> PathBuf,
) -> Result<&'arena [&'arena Declaration<'arena>], Vec<Error>> {
    let mut pass = interp::Interp::new(arena, interner);
    pass.evaluate(program, module_of)
}
