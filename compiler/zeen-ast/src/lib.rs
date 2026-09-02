pub mod declarations;
pub mod expressions;
pub mod statements;
pub mod types;

pub use declarations::{AliasDecl, Declaration, DeclarationKind};
pub use expressions::{Expression, ExpressionKind};
pub use statements::{Statement, StatementKind};
pub use types::{TypeExpr, TypeKind};

use miette::{NamedSource, SourceSpan};
use std::sync::Arc;

// NOTE: Zeen AST relies on external arena allocator (to avoid separated heap pointers like
// Box/Rc/Arc/...). So expressions/statements/declarations must keep lifetimed references to other
// members instead of "boxing" them on heap.

// NOTE: `Spur` is a key for `lasso` string interner.

#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub span: SourceSpan,
    pub src: NamedSource<Arc<String>>,
}

impl Source {
    pub fn src(&self) -> NamedSource<Arc<String>> {
        self.src.clone()
    }
}

impl From<(SourceSpan, NamedSource<Arc<String>>)> for Source {
    fn from(value: (SourceSpan, NamedSource<Arc<String>>)) -> Self {
        Self {
            span: value.0,
            src: value.1,
        }
    }
}

impl From<(NamedSource<Arc<String>>, SourceSpan)> for Source {
    fn from(value: (NamedSource<Arc<String>>, SourceSpan)) -> Self {
        Self {
            span: value.1,
            src: value.0,
        }
    }
}
