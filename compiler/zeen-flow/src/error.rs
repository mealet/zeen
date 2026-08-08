use smol_str::SmolStr;
use std::sync::Arc;

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

#[derive(Debug, Error, Diagnostic, Clone)]
pub enum FlowError {
    #[error("use of uninitialized value `{name}`")]
    #[diagnostic(
        severity(Error),
        code(zeen::dataflow::use_of_uninitialized),
        help("initialize `{name}` before using it")
    )]
    UseOfUninitialized {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("use of moved value `{name}`")]
    #[diagnostic(
        severity(Error),
        code(zeen::dataflow::use_after_move),
        help("value `{name}` was moved out, borrow or clone it instead")
    )]
    UseAfterMove {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("cannot move out of field of `{name}`: struct implements `Drop`")]
    #[diagnostic(
        severity(Error),
        code(zeen::dataflow::move_out_of_drop),
        help(
            "structs implementing `Drop` cannot be partially moved, drop would be called on a partial value"
        )
    )]
    MoveOutOfDrop {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("use of partially moved struct `{name}`")]
    #[diagnostic(
        severity(Error),
        code(zeen::dataflow::use_of_partially_moved),
        help("reinitialize all moved fields before using the struct as a whole")
    )]
    UseOfPartiallyMoved {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("cannot assign to constant `{name}`")]
    #[diagnostic(
        severity(Error),
        code(zeen::dataflow::assign_to_const),
        help("declare `{name}` with `var` to make it mutable")
    )]
    AssignToConst {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("unused variable `{name}`")]
    #[diagnostic(
        severity(Warning),
        code(zeen::dataflow::unused_variable),
        help("consider removing it, or prefixing the name with `_`")
    )]
    UnusedVariable {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("variable `{name}` does not need to be mutable")]
    #[diagnostic(
        severity(Warning),
        code(zeen::dataflow::unused_mut),
        help("replace the `var` keyword with `const` for `{name}`")
    )]
    UnusedMut {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },
}
