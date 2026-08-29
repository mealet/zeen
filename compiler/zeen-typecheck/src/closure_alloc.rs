//! Per-closure-site environment allocation analysis.
//!
//! Every closure expression becomes a fat `Fn`/`FnOnce` value backed by an
//! environment struct. Whether that environment lives on the stack of the
//! creating frame or is heap-allocated (`malloc`) is a static per-site
//! decision, computed here and consumed by MIR lowering (and, later, dataflow
//! and codegen).
//!
//! The rules are deliberately simple and conservative:
//!
//! - The closure expression is returned (return statement, function tail, or
//!   a branch tail that feeds a return) from its defining function -> `Heap`.
//! - The closure expression is passed as a call argument, stored into an
//!   aggregate, or otherwise flows into an escaping position -> `Heap`.
//! - The closure value is bound to a `let` local:
//!   - the local is never referenced (and not captured by a sibling closure)
//!     -> `Unused` (the backend must not materialize the value at all);
//!   - the local is referenced only as a call target -> `Stack`;
//!   - the local is referenced in any other position (moved into another
//!     local, passed along, returned, captured by a nested closure, ...)
//!     -> `Heap`.
//!
//! `Heap` never dangles; it may leak or over-allocate in a few cases that
//! later stages can refine.

use std::collections::{HashMap, HashSet};

use zeen_hir::{HirDeclKind, HirExpr, HirExprKind, HirModule, HirStmt, HirStmtKind};
use zeen_resolve::DefId;

/// How (and whether) a closure's captured environment is materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClosureAllocKind {
    /// The env is heap-allocated (`malloc`) because the closure value can
    /// outlive the creating frame.
    Heap,
    /// The env is a plain stack value of the creating frame.
    Stack,
    /// The closure value is never used; the backend must not create it.
    Unused,
}

/// What happens to the value a walked expression produces.
#[derive(Debug, Clone)]
enum Fate {
    /// The value is dropped (statement context).
    Discard,
    /// The value can leave the defining frame (return, argument, stored).
    Escaping,
    /// The value is being called right now.
    Callee,
    /// The value is bound to a `let` local.
    Bind(DefId),
}

#[derive(Debug, Default, Clone, Copy)]
struct LocalRefs {
    /// Referenced as a call target (and nothing else).
    callee: bool,
    /// Referenced in some escaping position (returned, passed, stored,
    /// captured, moved out).
    other: bool,
}

pub fn analyze_closures(
    module: &HirModule,
    captures: &HashMap<DefId, Vec<DefId>>,
) -> HashMap<DefId, ClosureAllocKind> {
    let mut analyzer = Analyzer {
        captures,
        locals: HashMap::new(),
        scope: Vec::new(),
        sites: HashMap::new(),
        allocs: HashMap::new(),
    };

    for decl in &module.decls {
        match &decl.kind {
            HirDeclKind::Fn(f) => analyzer.fn_body(&f.body),
            HirDeclKind::Struct(s) => {
                for method in &s.methods {
                    analyzer.decl_fn(method);
                }
            }
            HirDeclKind::Interface(i) => {
                for method in &i.methods {
                    analyzer.decl_fn(method);
                }
            }
            HirDeclKind::Implement(imp) => {
                for method in &imp.methods {
                    analyzer.decl_fn(method);
                }
            }
            HirDeclKind::GlobalVar { value, .. } => analyzer.expr(value, &Fate::Escaping),
            _ => {}
        }
    }

    analyzer.finish()
}

struct Analyzer<'a> {
    captures: &'a HashMap<DefId, Vec<DefId>>,
    /// Every let-bound local seen so far (keyed by its `DefId`).
    locals: HashMap<DefId, LocalRefs>,
    /// Lexical scope stack of currently-visible local `DefId`s.
    scope: Vec<HashSet<DefId>>,
    /// Closure `DefId` -> the local it was bound to.
    sites: HashMap<DefId, DefId>,
    allocs: HashMap<DefId, ClosureAllocKind>,
}

impl<'a> Analyzer<'a> {
    fn fn_body(&mut self, body: &Option<std::rc::Rc<HirStmt>>) {
        self.scope.push(HashSet::new());
        if let Some(body) = body {
            self.stmt_fate(body, &Fate::Escaping);
        }
        self.scope.pop();
    }

    fn decl_fn(&mut self, decl: &zeen_hir::HirDecl) {
        if let HirDeclKind::Fn(f) = &decl.kind {
            self.fn_body(&f.body);
        }
    }

    fn in_scope(&self, def_id: DefId) -> bool {
        self.scope.iter().rev().any(|frame| frame.contains(&def_id))
    }

    fn mark_ref(&mut self, def_id: DefId, callee: bool) {
        if !self.in_scope(def_id) {
            return;
        }

        let refs = self.locals.entry(def_id).or_default();
        if callee {
            refs.callee = true;
        } else {
            refs.other = true;
        }
    }

    fn stmt(&mut self, stmt: &HirStmt) {
        self.stmt_fate(stmt, &Fate::Discard);
    }

    fn stmt_fate(&mut self, stmt: &HirStmt, fate: &Fate) {
        match &stmt.kind {
            HirStmtKind::Let { def_id, value, .. } => {
                self.locals.entry(*def_id).or_default();
                self.scope.iter_mut().last().unwrap().insert(*def_id);

                if let Some(value) = value {
                    self.expr(value, &Fate::Bind(*def_id));
                }
            }

            HirStmtKind::Return { value } => {
                if let Some(value) = value {
                    self.expr(value, &Fate::Escaping);
                }
            }

            HirStmtKind::Assign { object, value }
            | HirStmtKind::CompoundAssign { object, value, .. } => {
                self.expr(object, &Fate::Discard);
                self.expr(value, &Fate::Escaping);
            }

            HirStmtKind::While { condition, block } => {
                self.expr(condition, &Fate::Discard);
                self.stmt(block);
            }

            HirStmtKind::For {
                def_id,
                iterator,
                block,
                ..
            } => {
                self.expr(iterator, &Fate::Discard);
                self.locals.entry(*def_id).or_default();
                self.scope.iter_mut().last().unwrap().insert(*def_id);
                self.stmt(block);
            }

            HirStmtKind::Expr(expr) => self.expr(expr, fate),

            HirStmtKind::FnDecl(decl) => self.decl_fn(decl),

            HirStmtKind::Break | HirStmtKind::Continue | HirStmtKind::Error => {}
        }
    }

    fn expr(&mut self, expr: &HirExpr, fate: &Fate) {
        match &expr.kind {
            HirExprKind::Closure { def_id, def } => {
                match fate {
                    Fate::Bind(local) => {
                        self.sites.insert(*def_id, *local);
                    }
                    Fate::Escaping => {
                        self.allocs.insert(*def_id, ClosureAllocKind::Heap);
                    }
                    Fate::Callee => {
                        self.allocs.insert(*def_id, ClosureAllocKind::Stack);
                    }
                    Fate::Discard => {
                        self.allocs.insert(*def_id, ClosureAllocKind::Unused);
                    }
                }

                // Nested closures live in their own function frame.
                self.scope.push(HashSet::new());
                if let Some(body) = &def.body {
                    self.stmt_fate(body, &Fate::Escaping);
                }
                self.scope.pop();
            }

            HirExprKind::Call {
                callee,
                args,
                generic_args: _,
            } => {
                self.expr(callee, &Fate::Callee);
                for arg in args {
                    self.expr(arg, &Fate::Escaping);
                }
            }

            HirExprKind::VarRef(def_id) => self.mark_ref(*def_id, matches!(fate, Fate::Callee)),

            HirExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.expr(condition, &Fate::Discard);
                self.stmt_fate(then_block, fate);
                if let Some(else_block) = else_block {
                    self.stmt_fate(else_block, fate);
                }
            }

            HirExprKind::Block { stmts, trailing } => {
                self.scope.push(HashSet::new());
                for stmt in stmts {
                    self.stmt(stmt);
                }
                if let Some(trailing) = trailing {
                    self.expr(trailing, fate);
                }
                self.scope.pop();
            }

            HirExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs, &Fate::Escaping);
                self.expr(rhs, &Fate::Escaping);
            }

            HirExprKind::Unary { expr: inner, .. } => self.expr(inner, &Fate::Escaping),

            HirExprKind::FieldAccess { object, .. } => self.expr(object, &Fate::Escaping),

            HirExprKind::SliceAccess { object, index } => {
                self.expr(object, &Fate::Escaping);
                self.expr(index, &Fate::Escaping);
            }

            HirExprKind::StructInit { fields, .. } => {
                for field in fields {
                    self.expr(&field.value, &Fate::Escaping);
                }
            }

            HirExprKind::ArrayInit { elements } => {
                for element in elements {
                    self.expr(element, &Fate::Escaping);
                }
            }

            HirExprKind::ArrayRepeatInit { element, len } => {
                self.expr(element, &Fate::Escaping);
                self.expr(len, &Fate::Discard);
            }

            HirExprKind::MacroCall { args, .. } => {
                for arg in args {
                    self.expr(arg, &Fate::Escaping);
                }
            }

            HirExprKind::Literal(_)
            | HirExprKind::GenericParamRef(_)
            | HirExprKind::SelfValue(_)
            | HirExprKind::Switch
            | HirExprKind::Type(_)
            | HirExprKind::Error => {}
        }
    }

    /// Resolves captured locals: a local captured by any closure is referenced
    /// from inside that closure's body, so its environment must survive as
    /// long as the capturing closure does (conservatively: `Heap`).
    fn finish(mut self) -> HashMap<DefId, ClosureAllocKind> {
        for captures in self.captures.values() {
            for captured in captures {
                self.locals.entry(*captured).or_default().other = true;
            }
        }

        for (closure_def, local) in &self.sites {
            let refs = self.locals.get(local).copied().unwrap_or_default();

            let kind = if !refs.callee && !refs.other {
                ClosureAllocKind::Unused
            } else if refs.other {
                ClosureAllocKind::Heap
            } else {
                ClosureAllocKind::Stack
            };

            self.allocs.insert(*closure_def, kind);
        }

        self.allocs
    }
}
