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
