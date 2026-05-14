#![allow(unused)]

pub mod declarations;
pub mod expressions;
pub mod statements;

// NOTE: Zeen AST relies on external arena allocator (to avoid separated heap pointers like
// Box/Rc/Arc/...). So expressions/statements/declarations must keep lifetimed references to other
// members instead of "boxing" them on heap.

// NOTE: `Spur` is a key for `lasso` string interner.
