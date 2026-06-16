use bumpalo::Bump;
use lasso::{Rodeo, Spur};
use miette::SourceSpan;
use smol_str::SmolStr;

use std::sync::{Arc, Mutex};

use zeen_ast::{
    declarations::{Declaration, DeclarationKind, GenericType},
    expressions::{Expression, ExpressionKind},
    statements::{Statement, StatementKind},
    types::{TypeExpr, TypeKind},
};

use crate::{
    error::ResolveError,
    resolution::{DefId, DefInfo, DefKind, NodeKey, Resolution, ResolutionResult},
    symbol_table::{ScopeKind, SymbolTable},
};

pub struct NameResolver<'ctx> {
    arena: &'ctx Bump,
    interner: Arc<Mutex<Rodeo>>,

    table: SymbolTable,
    result: ResolutionResult,
    next_def_id: u32,

    src: Arc<String>,
    filename: Arc<String>,
    errors: Vec<ResolveError>,
}

fn is_self_param(param: &zeen_ast::declarations::FnParam) -> bool {
    matches!(param.ty.kind, TypeKind::SelfType)
}

impl<'ctx> NameResolver<'ctx> {
    pub fn new(
        filename: Arc<String>,
        src: Arc<String>,

        arena: &'ctx Bump,
        interner: Arc<Mutex<Rodeo>>,
    ) -> Self {
        Self {
            arena,
            interner,

            table: SymbolTable::new(),
            result: ResolutionResult::default(),
            next_def_id: 0,

            src,
            filename,
            errors: Vec::new(),
        }
    }

    pub fn finish(self) -> Result<ResolutionResult, Vec<ResolveError>> {
        if !self.errors.is_empty() {
            return Err(self.errors);
        }

        Ok(self.result)
    }

    fn fresh_def_id(&mut self) -> DefId {
        let id = DefId(self.next_def_id);
        self.next_def_id += 1;
        id
    }

    fn define(&mut self, info: DefInfo) -> DefId {
        let id = self.fresh_def_id();
        self.result.defs.insert(id, info);
        id
    }

    fn named_src(&self) -> miette::NamedSource<Arc<String>> {
        let src_ref = Arc::clone(&self.src);

        miette::NamedSource::new(self.filename.as_str(), src_ref)
    }

    fn interner_intern(&mut self, value: impl AsRef<str>) -> lasso::Spur {
        // compiler is not async/threaded (at least for now), so we're unwrapping lock
        let mut interner = self.interner.lock().unwrap();

        interner.get_or_intern(value)
    }

    fn interner_resolve(&self, key: &Spur) -> SmolStr {
        let interner = self.interner.lock().unwrap();
        let resolved = interner.resolve(key);

        resolved.into()
    }

    fn report(&mut self, error: ResolveError) {
        self.errors.push(error);
    }

    // -> resolve functions

    pub fn resolve_module(
        mut self,
        decls: &'ctx [&'ctx Declaration<'ctx>],
    ) -> Result<(), Vec<ResolveError>> {
        for decl in decls {
            self.declare_toplevel(decl);
        }

        if !self.errors.is_empty() {
            return Err(self.errors);
        }

        for decl in decls {
            self.resolve_decl(decl);
        }

        if !self.errors.is_empty() {
            return Err(self.errors);
        }

        Ok(())
    }

    fn declare_toplevel(&mut self, decl: &'ctx Declaration<'ctx>) {
        match decl.kind {
            DeclarationKind::FnDecl { name, .. } => {
                let def_id = self.define(DefInfo {
                    name: name.0,
                    kind: DefKind::Function,
                    span: name.1,
                    decl: Some(NodeKey::from_decl(decl)),
                });

                self.table.declare_value(name.0, def_id);
            }

            DeclarationKind::StructDecl { name, .. } => {
                let def_id = self.define(DefInfo {
                    name: name.0,
                    kind: DefKind::Struct,
                    span: name.1,
                    decl: Some(NodeKey::from_decl(decl)),
                });

                self.table.declare_type(name.0, def_id);
            }

            DeclarationKind::InterfaceDecl { name, .. } => {
                let def_id = self.define(DefInfo {
                    name: name.0,
                    kind: DefKind::Interface,
                    span: name.1,
                    decl: Some(NodeKey::from_decl(decl)),
                });

                self.table.declare_type(name.0, def_id);
            }

            DeclarationKind::EnumDecl { name, variants, .. } => {
                let def_id = self.define(DefInfo {
                    name: name.0,
                    kind: DefKind::Enum,
                    span: name.1,
                    decl: Some(NodeKey::from_decl(decl)),
                });

                self.table.declare_type(name.0, def_id);

                for variant in variants {
                    let variant_id = self.define(DefInfo {
                        name: variant.name,
                        kind: DefKind::EnumVariant,
                        span: variant.span,
                        decl: Some(NodeKey::from_decl(decl)),
                    });

                    self.table.declare_value(variant.name, variant_id);
                }
            }

            DeclarationKind::ExternVar { name, .. } => {
                let def_id = self.define(DefInfo {
                    name: name.0,
                    kind: DefKind::ExternVar,
                    span: name.1,
                    decl: Some(NodeKey::from_decl(decl)),
                });

                self.table.declare_value(name.0, def_id);
            }

            DeclarationKind::ExternInclude { .. } => {
                self.errors.push(ResolveError::DisabledFeature {
                    reason: "not supported yet".into(),
                    src: self.named_src(),
                    span: decl.span,
                });
            }

            DeclarationKind::ExternLink { .. } => {}
            DeclarationKind::ImplementDecl { .. } => {}
            DeclarationKind::Use { .. } => {}
        }
    }

    // --> Declarations

    fn resolve_decl(&mut self, decl: &'ctx Declaration<'ctx>) {
        match decl.kind {
            DeclarationKind::FnDecl {
                generics,
                params,
                return_type,
                body,
                ..
            } => {
                self.table.push(ScopeKind::Function);
                self.declare_generics(generics);

                for param in params {
                    self.resolve_type(param.ty);

                    if let Some(name) = param.name {
                        let def_id = self.define(DefInfo {
                            name,
                            kind: DefKind::Param,
                            span: param.span,
                            decl: None,
                        });

                        self.table.declare_value(name, def_id);
                    }
                }

                if let Some(ret) = return_type {
                    self.resolve_type(ret);
                }

                if let Some(body) = body {
                    self.resolve_stmt(body);
                }

                self.table.pop();
            }

            DeclarationKind::StructDecl {
                name,
                generics,
                fields,
                methods,
                ..
            } => {
                let self_def = self
                    .table
                    .lookup_type(name.0)
                    .expect("struct is not registered in name resolver pass 1");

                self.table.push(ScopeKind::Block);
                self.declare_generics(generics);

                for field in fields {
                    self.resolve_type(field.ty);
                }

                for method in methods {
                    self.resolve_method(method, self_def);
                }

                self.table.pop();
            }

            DeclarationKind::InterfaceDecl {
                generics, methods, ..
            } => {
                self.table.push(ScopeKind::Block);
                self.declare_generics(generics);

                for method in methods {
                    self.resolve_decl(method);
                }

                self.table.pop();
            }

            DeclarationKind::ImplementDecl {
                interface,
                object,
                methods,
            } => {
                self.resolve_expr(interface);
                self.resolve_expr(object);

                let object_def = self.path_expr_to_type_def(object);
                let interface_def = self.path_expr_to_type_def(interface);

                let mut methods_ids = Vec::new();

                if let Some(self_def) = object_def {
                    for method in methods {
                        let method_id = self.resolve_method(method, self_def);
                        methods_ids.push(method_id);
                    }
                } else {
                    for method in methods {
                        self.resolve_decl(method);
                    }
                }

                if let (Some(obj), Some(iface)) = (object_def, interface_def) {
                    self.result.impls.insert((obj, iface), methods_ids);
                }
            }

            DeclarationKind::EnumDecl { .. } => {
                // nothing to resolve (for now at least)
            }

            DeclarationKind::ExternVar { ty, .. } => {
                self.resolve_type(ty);
            }

            DeclarationKind::ExternLink { .. } | DeclarationKind::ExternInclude { .. } => {}
            DeclarationKind::Use { .. } => {}
        }
    }

    fn resolve_method(&mut self, method: &'ctx Declaration<'ctx>, self_def: DefId) -> DefId {
        let DeclarationKind::FnDecl {
            name,
            generics,
            params,
            return_type,
            body,
            ..
        } = method.kind
        else {
            unreachable!("method must be FnDecl")
        };

        let method_id = self.define(DefInfo {
            name: name.0,
            kind: DefKind::Function,
            span: name.1,
            decl: Some(NodeKey::from_decl(method)),
        });

        let self_param = params
            .first()
            .filter(|param| is_self_param(param))
            .and_then(|param| param.name);

        let self_param_id = self_param.map(|name| {
            self.define(DefInfo {
                name,
                kind: DefKind::Param,
                span: method.span,
                decl: None,
            })
        });

        self.table.push(ScopeKind::Method {
            self_def,
            self_param: self_param_id,
        });
        self.declare_generics(generics);

        for param in params {
            self.resolve_type(param.ty);

            let Some(pname) = param.name else { continue };

            if is_self_param(param) {
                if let Some(id) = self_param_id {
                    self.table.declare_value(pname, id);
                }
                continue;
            }

            let def_id = self.define(DefInfo {
                name: pname,
                kind: DefKind::Param,
                span: param.span,
                decl: None,
            });
            self.table.declare_value(pname, def_id);
        }

        if let Some(ret) = return_type {
            self.resolve_type(ret);
        }

        if let Some(body) = body {
            self.resolve_stmt(body);
        }

        self.table.pop();

        method_id
    }

    fn path_expr_to_type_def(&self, expr: &Expression) -> Option<DefId> {
        match expr.kind {
            ExpressionKind::Ident { name, .. } => self.table.lookup_type(name),
            ExpressionKind::FieldAccess { field, .. } => {
                if let ExpressionKind::Ident { name, .. } = field.kind {
                    self.table.lookup_type(name)
                } else {
                    None
                }
            }

            _ => None,
        }
    }

    fn declare_generics(&mut self, generics: Option<&[GenericType]>) {
        let Some(generics) = generics else { return };

        for generic in generics {
            let def_id = self.define(DefInfo {
                name: generic.name.0,
                kind: DefKind::GenericParam,
                span: generic.name.1,
                decl: None,
            });

            self.table.declare_type(generic.name.0, def_id);

            if let Some(bounds) = generic.interfaces {
                for bound in bounds {
                    if self.table.lookup_type(bound.0).is_none() {
                        let bound_str = self.interner_resolve(&bound.0);

                        self.errors.push(ResolveError::UnresolvedType {
                            name: bound_str,
                            src: self.named_src(),
                            span: bound.1,
                        });
                    }
                }
            }
        }
    }

    // --> Statements

    fn resolve_stmt(&mut self, stmt: &'ctx Statement<'ctx>) {
        match stmt.kind {
            StatementKind::Let {
                name,
                explicit_type,
                value,
                is_const,
            } => {
                if let Some(ty) = explicit_type {
                    self.resolve_type(ty);
                }

                if let Some(value) = value {
                    self.resolve_expr(value);
                }

                let def_id = self.define(DefInfo {
                    name,
                    kind: DefKind::Variable { is_const },
                    span: stmt.span,
                    decl: None,
                });
                self.table.declare_value(name, def_id);

                self.result
                    .expr_bindings
                    .insert(NodeKey::from_stmt(stmt), Resolution::Def(def_id));
            }

            _ => todo!("all statements must be implemented!"),
        }
    }

    // --> Expressions

    fn resolve_expr(&mut self, expr: &'ctx Expression<'ctx>) {
        todo!()
    }

    // --> Types

    fn resolve_type(&mut self, expr: &'ctx TypeExpr<'ctx>) {
        todo!()
    }
}
