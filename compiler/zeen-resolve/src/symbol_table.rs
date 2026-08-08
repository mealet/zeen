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
