#![allow(unused)]

pub mod declarations;
pub mod expressions;
pub mod statements;
pub mod types;

pub use declarations::{Declaration, DeclarationKind};
pub use expressions::{Expression, ExpressionKind};
pub use statements::{Statement, StatementKind};
pub use types::{TypeExpr, TypeKind};

// NOTE: Zeen AST relies on external arena allocator (to avoid separated heap pointers like
// Box/Rc/Arc/...). So expressions/statements/declarations must keep lifetimed references to other
// members instead of "boxing" them on heap.

// NOTE: `Spur` is a key for `lasso` string interner.

// TODO: Add generic types support
