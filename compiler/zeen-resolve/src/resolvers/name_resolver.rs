use bumpalo::Bump;
use lasso::{Rodeo, Spur};
use miette::SourceSpan;

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
        todo!()
    }

    // --> Statements

    fn resolve_stmt(&mut self, stmt: &'ctx Statement<'ctx>) {
        todo!()
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
