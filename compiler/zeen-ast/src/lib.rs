pub mod declarations;
pub mod expressions;
pub mod statements;
pub mod types;

pub use declarations::{
    AliasDecl, ConditionalBlock, Declaration, DeclarationKind, DirectiveValue,
    PreprocessorDirective,
};
pub use expressions::{ExprConditionalBlock, Expression, ExpressionKind};
pub use statements::{Statement, StatementKind, StmtConditionalBlock};
pub use types::{TypeExpr, TypeKind};

use miette::{NamedSource, SourceSpan};
use std::sync::Arc;

// The AST lives in an external arena allocator instead of `Box`/`Rc`, so
// nodes hold lifetimed references to each other.

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
