use lasso::Spur;
use std::collections::HashMap;

use crate::resolution::DefId;

// --> Scope

#[derive(Debug, Clone, Copy)]
pub enum ScopeKind {
    Module,
    Function,
    Block,
    Method {
        self_def: DefId,
        self_param: Option<DefId>,
    },
    InterfaceMethod {
        self_placeholder: DefId,
        self_param: Option<DefId>,
    },
}

#[derive(Debug, Default, Clone)]
pub struct ScopeContent {
    pub values: HashMap<Spur, DefId>,
    pub types: HashMap<Spur, DefId>,
}

#[derive(Debug, Clone)]
pub struct Scope {
    content: ScopeContent,
    kind: ScopeKind,
}

impl Scope {
    pub fn new(content: ScopeContent, kind: ScopeKind) -> Self {
        Self { content, kind }
    }
}

// --> Symbol Table

pub struct SymbolTable {
    scopes: Vec<Scope>,
}

// (self / Self) defs
type SelfDefs = (DefId, Option<DefId>);

impl SymbolTable {
    pub fn new() -> Self {
        let init_scope = Scope::new(ScopeContent::default(), ScopeKind::Module);

        Self {
            scopes: vec![init_scope],
        }
    }

    pub fn push(&mut self, kind: ScopeKind) {
        self.scopes.push(Scope::new(ScopeContent::default(), kind));
    }

    pub fn pop(&mut self) {
        debug_assert!(self.scopes.len() > 1, "cannot pop module scope");
        self.scopes.pop();
    }

    pub fn current_mut(&mut self) -> &mut ScopeContent {
        &mut self.scopes.last_mut().expect("something wrong wtf").content
    }

    pub fn enclosing_method_or_interface(&self) -> Option<SelfDefs> {
        for scope in self.scopes.iter().rev() {
            match scope.kind {
                ScopeKind::Method {
                    self_def,
                    self_param,
                } => return Some((self_def, self_param)),

                ScopeKind::InterfaceMethod {
                    self_placeholder,
                    self_param,
                } => return Some((self_placeholder, self_param)),

                _ => {}
            }
        }

        None
    }

    // --> Lookups

    pub fn lookup_value(&self, name: Spur) -> Option<DefId> {
        for scope in self.scopes.iter().rev() {
            if let Some(id) = scope.content.values.get(&name) {
                return Some(*id);
            }
        }

        None
    }

    pub fn lookup_type(&self, name: Spur) -> Option<DefId> {
        for scope in self.scopes.iter().rev() {
            if let Some(id) = scope.content.types.get(&name) {
                return Some(*id);
            }
        }

        None
    }

    // --> Declarations

    pub fn declare_value(&mut self, name: Spur, id: DefId) {
        self.current_mut().values.insert(name, id);
    }

    pub fn declare_type(&mut self, name: Spur, id: DefId) {
        self.current_mut().types.insert(name, id);
    }

    /// Collects every `DefId` (values and types) visible in all scopes above the
    /// module one (the enclosing function's params, locals and generics,
    /// including the current block). Used to forbid nested functions from
    /// capturing them.
    pub fn enclosing_defs(&self) -> std::collections::HashSet<DefId> {
        let mut out = std::collections::HashSet::new();

        for scope in self.scopes.iter().rev() {
            match scope.kind {
                ScopeKind::Module => break,
                _ => {
                    out.extend(scope.content.values.values().copied());
                    out.extend(scope.content.types.values().copied());
                }
            }
        }

        out
    }

    /// Collects the `DefId`s a closure is allowed to capture: everything in the
    /// enclosing function's live frame (params, locals, generics) — all scopes
    /// outside the closure's own scope down to and including the first
    /// function-like scope. Globals are excluded (always reachable, never
    /// captured), and frames above the enclosing function are dead.
    pub fn closure_capture_candidates(&self) -> std::collections::HashSet<DefId> {
        let mut out = std::collections::HashSet::new();

        let mut scopes = self.scopes.iter().rev();
        // Skip the closure's own scope: its params/generics are locals, not
        // captures.
        scopes.next();

        for scope in scopes {
            match scope.kind {
                ScopeKind::Module => break,

                ScopeKind::Block => {
                    out.extend(scope.content.values.values().copied());
                    out.extend(scope.content.types.values().copied());
                }

                // Enclosing function/method frame: its params (incl. `self`
                // and env params of outer closures) are capturable. Frames
                // above it are not live, so stop here.
                _ => {
                    out.extend(scope.content.values.values().copied());
                    out.extend(scope.content.types.values().copied());
                    break;
                }
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::Rodeo;
    use std::{cell::RefCell, rc::Rc};

    fn intern(rodeo: &Rc<RefCell<Rodeo>>, string: &str) -> Spur {
        rodeo.borrow_mut().get_or_intern(string)
    }

    #[test]
    fn declared_value_is_visible_in_own_scope() {
        let rodeo = Rc::new(RefCell::new(Rodeo::default()));
        let mut table = SymbolTable::new();

        let name = intern(&rodeo, "foo");
        table.declare_value(name, DefId(1));

        assert_eq!(table.lookup_value(name), Some(DefId(1)));
        assert_eq!(table.lookup_type(name), None);
    }

    #[test]
    fn values_and_types_are_separate_namespaces() {
        let rodeo = Rc::new(RefCell::new(Rodeo::default()));
        let mut table = SymbolTable::new();

        let name = intern(&rodeo, "foo");
        table.declare_type(name, DefId(1));

        assert_eq!(table.lookup_type(name), Some(DefId(1)));
        assert_eq!(table.lookup_value(name), None);
    }

    #[test]
    fn inner_scope_shadows_outer_until_pop() {
        let rodeo = Rc::new(RefCell::new(Rodeo::default()));
        let mut table = SymbolTable::new();

        let name = intern(&rodeo, "foo");
        table.declare_value(name, DefId(1));

        table.push(ScopeKind::Block);
        table.declare_value(name, DefId(2));

        assert_eq!(table.lookup_value(name), Some(DefId(2)));

        table.pop();
        assert_eq!(table.lookup_value(name), Some(DefId(1)));
    }

    #[test]
    fn lookup_searches_outwards_from_current_scope() {
        let rodeo = Rc::new(RefCell::new(Rodeo::default()));
        let mut table = SymbolTable::new();

        let outer = intern(&rodeo, "outer");
        let inner = intern(&rodeo, "inner");

        table.declare_value(outer, DefId(1));

        table.push(ScopeKind::Function);
        table.declare_value(inner, DefId(2));

        assert_eq!(table.lookup_value(outer), Some(DefId(1)));
        assert_eq!(table.lookup_value(inner), Some(DefId(2)));

        table.pop();
        assert_eq!(table.lookup_value(inner), None);
    }

    #[test]
    fn enclosing_method_scope_is_found_inside_nested_blocks() {
        let mut table = SymbolTable::new();

        table.push(ScopeKind::Method {
            self_def: DefId(10),
            self_param: Some(DefId(11)),
        });
        table.push(ScopeKind::Block);

        assert_eq!(
            table.enclosing_method_or_interface(),
            Some((DefId(10), Some(DefId(11))))
        );
    }

    #[test]
    fn enclosing_interface_method_scope_is_found() {
        let mut table = SymbolTable::new();

        table.push(ScopeKind::InterfaceMethod {
            self_placeholder: DefId(20),
            self_param: None,
        });

        assert_eq!(
            table.enclosing_method_or_interface(),
            Some((DefId(20), None))
        );
    }

    #[test]
    fn no_method_scope_returns_none() {
        let mut table = SymbolTable::new();
        table.push(ScopeKind::Block);

        assert_eq!(table.enclosing_method_or_interface(), None);
    }
}
