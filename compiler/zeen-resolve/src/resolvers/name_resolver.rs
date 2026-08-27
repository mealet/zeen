use lasso::{Rodeo, Spur};
use miette::{NamedSource, SourceSpan};
use smol_str::SmolStr;

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::Arc,
};

use zeen_ast::{
    declarations::{Declaration, DeclarationKind, GenericType},
    expressions::{Expression, ExpressionKind},
    statements::{Statement, StatementKind},
    types::{TypeExpr, TypeKind},
};

use crate::{
    error::ResolveError,
    resolution::{BindingSlotKey, DefId, DefInfo, DefKind, NodeKey, Resolution, ResolutionResult},
    same_source_file,
    symbol_table::{ScopeKind, SymbolTable},
};

pub struct NameResolver {
    interner: Rc<RefCell<Rodeo>>,
    errors: Vec<ResolveError>,

    table: SymbolTable,
    result: ResolutionResult,

    /// DefIds of enclosing-function params/locals a nested function is not
    /// allowed to reference (no closures). One entry per active nested fn body.
    capture_stack: Vec<HashSet<DefId>>,

    /// The `DefId` of the function whose body is currently being resolved,
    /// used to record the parent of nested function declarations.
    current_fn_def: Option<DefId>,

    /// Edges of the global variables dependency graph: a global var -> globals
    /// referenced from its initializer expression.
    global_deps: HashMap<DefId, Vec<DefId>>,

    next_def_id: u32,
    current_src: NamedSource<Arc<String>>,
}

fn is_self_param(param: &zeen_ast::declarations::FnParam) -> bool {
    fn is_self_inner(ty: &TypeKind) -> bool {
        match ty {
            TypeKind::SelfType => true,
            TypeKind::Const(inner) => is_self_inner(&inner.kind),
            TypeKind::SinglePointer(inner) => is_self_inner(&inner.kind),
            _ => false,
        }
    }

    is_self_inner(&param.ty.kind)
}

impl<'ctx> NameResolver {
    pub fn new(filename: Rc<String>, src: Arc<String>, interner: Rc<RefCell<Rodeo>>) -> Self {
        Self {
            interner,
            errors: Vec::new(),

            table: SymbolTable::new(),
            result: ResolutionResult::default(),

            capture_stack: Vec::new(),
            current_fn_def: None,
            global_deps: HashMap::new(),

            next_def_id: 0,
            current_src: NamedSource::new(filename.as_str(), src.clone()),
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

    fn define_at(&mut self, key: NodeKey, info: DefInfo) -> DefId {
        let id = self.define(info);
        self.result.binding_sites.insert(key, id);
        id
    }

    fn named_src(&self) -> miette::NamedSource<Arc<String>> {
        self.current_src.clone()
    }

    fn interner_intern(&mut self, value: impl AsRef<str>) -> lasso::Spur {
        let mut interner = self.interner.borrow_mut();
        interner.get_or_intern(value)
    }

    fn interner_resolve(&self, key: &Spur) -> SmolStr {
        let interner = self.interner.borrow();
        let resolved = interner.resolve(key);
        resolved.into()
    }

    fn check_visibility(&mut self, def_id: DefId, source: &zeen_ast::Source) {
        let Some(info) = self.result.defs.get(&def_id) else {
            return;
        };
        if info.is_pub {
            return;
        }

        if !same_source_file(&info.span.src(), &self.current_src) {
            let interner = self.interner.borrow();

            let name = interner.resolve(&info.name).into();

            drop(interner);

            self.report(ResolveError::PrivateItemNotAccessible {
                name,
                src: source.src(),
                span: source.span,
            });
        }
    }

    fn report(&mut self, error: ResolveError) {
        self.errors.push(error);
    }

    // -> resolve functions

    pub fn resolve_module(&mut self, decls: &'ctx [&'ctx Declaration<'ctx>]) {
        let old_reports = self.errors.len();

        for decl in decls {
            self.declare_toplevel(decl);
        }

        if self.errors.len() > old_reports {
            return;
        }

        for decl in decls {
            self.resolve_decl(decl);
        }

        self.check_global_var_cycles();
    }

    fn declare_toplevel(&mut self, decl: &'ctx Declaration<'ctx>) {
        match decl.kind {
            DeclarationKind::FnDecl { name, is_pub, .. } => {
                let def_id = self.define_at(
                    NodeKey::from_decl(decl),
                    DefInfo {
                        name: name.0,
                        kind: DefKind::Function,
                        span: (name.1, decl.source.src()).into(),
                        decl: Some(NodeKey::from_decl(decl)),
                        is_pub,
                    },
                );

                self.table.declare_value(name.0, def_id);
            }

            DeclarationKind::StructDecl { name, is_pub, .. } => {
                let def_id = self.define_at(
                    NodeKey::from_decl(decl),
                    DefInfo {
                        name: name.0,
                        kind: DefKind::Struct,
                        span: (name.1, decl.source.src()).into(),
                        decl: Some(NodeKey::from_decl(decl)),
                        is_pub,
                    },
                );

                self.table.declare_type(name.0, def_id);
            }

            DeclarationKind::InterfaceDecl { name, is_pub, .. } => {
                let _name_resolved = self.interner_resolve(&name.0);

                let def_id = self.define_at(
                    NodeKey::from_decl(decl),
                    DefInfo {
                        name: name.0,
                        kind: DefKind::Interface,
                        span: (name.1, decl.source.src()).into(),
                        decl: Some(NodeKey::from_decl(decl)),
                        is_pub,
                    },
                );

                self.table.declare_type(name.0, def_id);

                let self_placeholder = self.define(DefInfo {
                    name: name.0,
                    kind: DefKind::InterfaceSelfPlaceholder,
                    span: (name.1, decl.source.src()).into(),
                    decl: None,
                    is_pub: false,
                });

                self.result
                    .interface_self_placeholders
                    .insert(def_id, self_placeholder);
            }

            DeclarationKind::EnumDecl {
                name,
                variants,
                is_pub,
                ..
            } => {
                let def_id = self.define_at(
                    NodeKey::from_decl(decl),
                    DefInfo {
                        name: name.0,
                        kind: DefKind::Enum,
                        span: (name.1, decl.source.src()).into(),
                        decl: Some(NodeKey::from_decl(decl)),
                        is_pub,
                    },
                );

                self.table.declare_type(name.0, def_id);

                for variant in variants {
                    let variant_id = self.define_at(
                        NodeKey::from_variant(variant),
                        DefInfo {
                            name: variant.name,
                            kind: DefKind::EnumVariant,
                            span: (variant.span, decl.source.src()).into(),
                            decl: Some(NodeKey::from_decl(decl)),
                            is_pub: true,
                        },
                    );

                    self.table.declare_value(variant.name, variant_id);
                }
            }

            DeclarationKind::ExternVar { name, .. } => {
                let def_id = self.define_at(
                    NodeKey::from_decl(decl),
                    DefInfo {
                        name: name.0,
                        kind: DefKind::ExternVar,
                        span: (name.1, decl.source.src()).into(),
                        decl: Some(NodeKey::from_decl(decl)),
                        is_pub: false,
                    },
                );

                self.table.declare_value(name.0, def_id);
            }

            DeclarationKind::GlobalVar {
                name,
                is_const,
                is_pub,
                ..
            } => {
                let def_id = self.define_at(
                    NodeKey::from_decl(decl),
                    DefInfo {
                        name: name.0,
                        kind: DefKind::GlobalVar { is_const },
                        span: (name.1, decl.source.src()).into(),
                        decl: Some(NodeKey::from_decl(decl)),
                        is_pub,
                    },
                );

                self.table.declare_value(name.0, def_id);
            }

            DeclarationKind::ExternInclude { .. } => {
                self.report(ResolveError::DisabledFeature {
                    reason: "not supported yet".into(),
                    src: decl.source.src(),
                    span: decl.source.span,
                });
            }

            DeclarationKind::ExternLink { .. } => {}
            DeclarationKind::ImplementDecl { .. } => {}
            DeclarationKind::Use { .. } => {}
        }
    }

    // --> Declarations

    fn resolve_decl(&mut self, decl: &'ctx Declaration<'ctx>) {
        self.current_src = decl.source.src();

        match decl.kind {
            DeclarationKind::FnDecl { .. } => {
                let prev_fn = self.current_fn_def;
                self.current_fn_def = self
                    .result
                    .binding_sites
                    .get(&NodeKey::from_decl(decl))
                    .copied();

                self.resolve_fn_body(decl);

                self.current_fn_def = prev_fn;
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
                self.declare_generics(generics, &decl.source.src);

                for field in fields {
                    self.resolve_type(field.ty);

                    self.define_at(
                        NodeKey::from_field(field),
                        DefInfo {
                            name: field.name,
                            kind: DefKind::Field,
                            span: ((0, 0).into(), decl.source.src()).into(),
                            decl: Some(NodeKey::from_decl(decl)),
                            is_pub: field.is_pub,
                        },
                    );
                }

                for method in methods {
                    self.resolve_method(method, self_def);
                }

                self.table.pop();
            }

            DeclarationKind::InterfaceDecl {
                generics, methods, ..
            } => {
                let iface_def = self
                    .result
                    .binding_sites
                    .get(&NodeKey::from_decl(decl))
                    .copied()
                    .expect("interface must be registered in pass 1");

                let self_placeholder = self.result.interface_self_placeholders[&iface_def];

                self.table.push(ScopeKind::Block);
                self.declare_generics(generics, &decl.source.src);

                for method in methods {
                    self.resolve_interface_method(method, self_placeholder);
                }

                self.table.pop();
            }

            DeclarationKind::ImplementDecl {
                interface,
                object,
                methods,
                generics,
            } => {
                self.table.push(ScopeKind::Block);
                self.declare_generics(generics, &decl.source.src);

                let interface_def = self.table.lookup_type(interface.0);

                if interface_def.is_none() {
                    let interface_name = self.interner_resolve(&interface.0);

                    self.report(ResolveError::UnresolvedType {
                        name: interface_name,
                        src: decl.source.src(),
                        span: interface.1,
                    });
                }

                let (object_name, object_span, object_bindings) = object;
                let object_def = self.table.lookup_type(object_name);

                if object_def.is_none() {
                    let object_name = self.interner_resolve(&object_name);

                    self.report(ResolveError::UnresolvedType {
                        name: object_name,
                        src: decl.source.src(),
                        span: object_span,
                    });
                }

                self.result.implement_names.insert(
                    NodeKey::from_decl(decl),
                    (
                        interface_def
                            .map(Resolution::Def)
                            .unwrap_or(Resolution::Error),
                        object_def.map(Resolution::Def).unwrap_or(Resolution::Error),
                    ),
                );

                for (idx, (binding_name, binding_span)) in object_bindings.iter().enumerate() {
                    let resolution = match self.table.lookup_type(*binding_name) {
                        Some(def_id) => Resolution::Def(def_id),
                        None => {
                            let name = self.interner_resolve(binding_name);

                            self.report(ResolveError::UnresolvedType {
                                name,
                                src: decl.source.src(),
                                span: *binding_span,
                            });

                            Resolution::Error
                        }
                    };

                    self.result
                        .implement_generic_bindings
                        .insert(BindingSlotKey(decl as *const _ as usize, idx), resolution);
                }

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

                self.table.pop();
            }

            DeclarationKind::EnumDecl { .. } => {
                // nothing to resolve (for now at least)
            }

            DeclarationKind::ExternVar { ty, .. } => {
                self.resolve_type(ty);
            }

            DeclarationKind::GlobalVar { ty, value, .. } => {
                self.resolve_type(ty);
                self.resolve_expr(value);

                if let Some(def_id) = self
                    .result
                    .binding_sites
                    .get(&NodeKey::from_decl(decl))
                    .copied()
                {
                    let mut deps = Vec::new();
                    self.collect_global_deps(value, &mut deps);

                    if !deps.is_empty() {
                        self.global_deps.insert(def_id, deps);
                    }
                }
            }

            DeclarationKind::ExternLink { .. } | DeclarationKind::ExternInclude { .. } => {}
            DeclarationKind::Use { .. } => {}
        }
    }

    // --> Global variables dependency graph

    fn collect_global_deps(&mut self, expr: &'ctx Expression<'ctx>, out: &mut Vec<DefId>) {
        match expr.kind {
            ExpressionKind::Ident { .. } => {
                if let Some(Resolution::Def(id)) = self.result.resolution_of_expr(expr)
                    && matches!(
                        self.result.defs.get(&id).map(|info| &info.kind),
                        Some(DefKind::GlobalVar { .. })
                    )
                    && !out.contains(&id)
                {
                    out.push(id);
                }
            }

            ExpressionKind::Binary { lhs, rhs, .. } => {
                self.collect_global_deps(lhs, out);
                self.collect_global_deps(rhs, out);
            }

            ExpressionKind::Unary { expr: inner, .. } => self.collect_global_deps(inner, out),

            ExpressionKind::Call { callee, args } => {
                self.collect_global_deps(callee, out);
                for arg in args {
                    self.collect_global_deps(arg, out);
                }
            }

            ExpressionKind::MacroCall { args, .. } => {
                for arg in args {
                    self.collect_global_deps(arg, out);
                }
            }

            ExpressionKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.collect_global_deps(condition, out);
                self.collect_global_stmt_deps(then_block, out);

                if let Some(else_block) = else_block {
                    self.collect_global_stmt_deps(else_block, out);
                }
            }

            ExpressionKind::Switch { object, .. } => self.collect_global_deps(object, out),

            ExpressionKind::FieldAccess { object, .. } => self.collect_global_deps(object, out),

            ExpressionKind::SliceAccess { object, index } => {
                self.collect_global_deps(object, out);
                self.collect_global_deps(index, out);
            }

            ExpressionKind::StructInit { ty, fields } => {
                self.collect_global_deps(ty, out);

                if let Some(fields) = fields {
                    for field in fields {
                        self.collect_global_deps(field.value, out);
                    }
                }
            }

            ExpressionKind::ArrayInit { elements } => {
                for elem in elements {
                    self.collect_global_deps(elem, out);
                }
            }

            ExpressionKind::Block { stmts, trailing } => {
                for stmt in stmts {
                    self.collect_global_stmt_deps(stmt, out);
                }

                if let Some(trailing) = trailing {
                    self.collect_global_deps(trailing, out);
                }
            }

            ExpressionKind::Type(ty) => {
                if let TypeKind::Array { len: Some(len), .. } = ty.kind {
                    self.collect_global_deps(len, out);
                }
            }

            ExpressionKind::Literal(_) => {}
        }
    }

    fn collect_global_stmt_deps(&mut self, stmt: &'ctx Statement<'ctx>, out: &mut Vec<DefId>) {
        match stmt.kind {
            StatementKind::Let { value, .. } => {
                if let Some(value) = value {
                    self.collect_global_deps(value, out);
                }
            }

            StatementKind::Assign { object, value } => {
                self.collect_global_deps(object, out);
                self.collect_global_deps(value, out);
            }

            StatementKind::CompoundAssign { object, value, .. } => {
                self.collect_global_deps(object, out);
                self.collect_global_deps(value, out);
            }

            StatementKind::Return { value } => {
                if let Some(value) = value {
                    self.collect_global_deps(value, out);
                }
            }

            StatementKind::While { condition, block } => {
                self.collect_global_deps(condition, out);
                self.collect_global_stmt_deps(block, out);
            }

            StatementKind::For {
                iterator, block, ..
            } => {
                self.collect_global_deps(iterator, out);
                self.collect_global_stmt_deps(block, out);
            }

            StatementKind::FnDecl(_) => {}

            StatementKind::Expr(expr) => self.collect_global_deps(expr, out),

            StatementKind::Break | StatementKind::Continue => {}
            StatementKind::TrailingExpr(_) => panic!("that was not supposed to happen"),
        }
    }

    fn check_global_var_cycles(&mut self) {
        // 0 = unvisited, 1 = in progress, 2 = done
        let mut state: HashMap<DefId, u8> = HashMap::new();
        let mut stack: Vec<DefId> = Vec::new();

        let mut nodes: Vec<DefId> = self.global_deps.keys().copied().collect();
        nodes.sort();

        for node in nodes {
            if state.get(&node).copied().unwrap_or(0) != 0 {
                continue;
            }

            if let Some(cycle) = self.visit_global(node, &mut state, &mut stack) {
                let names: Vec<SmolStr> = cycle
                    .iter()
                    .filter_map(|id| {
                        self.result
                            .defs
                            .get(id)
                            .map(|info| self.interner_resolve(&info.name))
                    })
                    .collect();

                let mut chain = String::new();
                for name in &names {
                    chain.push_str(name.as_str());
                    chain.push_str(" -> ");
                }
                chain.push_str(names.first().map(|n| n.as_str()).unwrap_or("?"));

                let info = self.result.defs.get(&cycle[0]).cloned().unwrap_or(DefInfo {
                    name: Spur::default(),
                    kind: DefKind::GlobalVar { is_const: false },
                    span: (SourceSpan::new(0.into(), 0), self.named_src()).into(),
                    decl: None,
                    is_pub: false,
                });

                self.report(ResolveError::GlobalVarCycle {
                    chain: chain.into(),
                    src: info.span.src(),
                    span: info.span.span,
                });

                return;
            }
        }
    }

    fn visit_global(
        &mut self,
        node: DefId,
        state: &mut HashMap<DefId, u8>,
        stack: &mut Vec<DefId>,
    ) -> Option<Vec<DefId>> {
        match state.get(&node).copied().unwrap_or(0) {
            1 => {
                let pos = stack.iter().position(|&n| n == node)?;
                return Some(stack[pos..].to_vec());
            }
            2 => return None,
            _ => {}
        }

        state.insert(node, 1);
        stack.push(node);

        let deps = self.global_deps.get(&node).cloned().unwrap_or_default();
        for dep in deps {
            if let Some(cycle) = self.visit_global(dep, state, stack) {
                return Some(cycle);
            }
        }

        stack.pop();
        state.insert(node, 2);
        None
    }

    fn resolve_fn_body(&mut self, decl: &'ctx Declaration<'ctx>) {
        let DeclarationKind::FnDecl {
            generics,
            params,
            return_type,
            body,
            ..
        } = decl.kind
        else {
            unreachable!("resolve_fn_body called on non-FnDecl")
        };

        self.table.push(ScopeKind::Function);
        self.declare_generics(generics, &decl.source.src);

        for param in params {
            self.resolve_type(param.ty);

            if let Some(name) = param.name {
                let def_id = self.define_at(
                    NodeKey::from_param(param),
                    DefInfo {
                        name,
                        kind: DefKind::Param,
                        span: (param.span, decl.source.src()).into(),
                        decl: None,
                        is_pub: false,
                    },
                );

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

    fn resolve_method(&mut self, method: &'ctx Declaration<'ctx>, self_def: DefId) -> DefId {
        let DeclarationKind::FnDecl {
            name,
            generics,
            params,
            return_type,
            body,
            is_pub,
            ..
        } = method.kind
        else {
            unreachable!("method must be FnDecl")
        };

        let method_id = self.define_at(
            NodeKey::from_decl(method),
            DefInfo {
                name: name.0,
                kind: DefKind::Function,
                span: (name.1, self.named_src()).into(),
                decl: Some(NodeKey::from_decl(method)),
                is_pub,
            },
        );

        let self_param = params.first().filter(|param| is_self_param(param));
        let self_intern = self.interner_intern("self");

        let self_param_id = self_param.map(|p| {
            self.define_at(
                NodeKey::from_param(p),
                DefInfo {
                    name: p.name.unwrap_or(self_intern),
                    kind: DefKind::Param,
                    span: method.source.clone(),
                    decl: None,
                    is_pub: false,
                },
            )
        });

        self.table.push(ScopeKind::Method {
            self_def,
            self_param: self_param_id,
        });
        self.declare_generics(generics, &method.source.src);

        for param in params {
            self.resolve_type(param.ty);

            let Some(pname) = param.name else { continue };

            if is_self_param(param) {
                if let Some(id) = self_param_id {
                    self.table.declare_value(pname, id);
                }
                continue;
            }

            let def_id = self.define_at(
                NodeKey::from_param(param),
                DefInfo {
                    name: pname,
                    kind: DefKind::Param,
                    span: (param.span, method.source.src()).into(),
                    decl: None,
                    is_pub: false,
                },
            );
            self.table.declare_value(pname, def_id);
        }

        if let Some(ret) = return_type {
            self.resolve_type(ret);
        }

        let prev_fn = self.current_fn_def;
        self.current_fn_def = Some(method_id);

        if let Some(body) = body {
            self.resolve_stmt(body);
        }

        self.current_fn_def = prev_fn;

        self.table.pop();

        method_id
    }

    fn resolve_interface_method(
        &mut self,
        method: &'ctx Declaration,
        self_placeholder: DefId,
    ) -> DefId {
        let DeclarationKind::FnDecl {
            name,
            generics,
            params,
            return_type,
            body,
            is_pub,
            ..
        } = &method.kind
        else {
            unreachable!("inetrface methods must be FnDecl")
        };

        let method_id = self.define_at(
            NodeKey::from_decl(method),
            DefInfo {
                name: name.0,
                kind: DefKind::Function,
                span: (name.1, method.source.src()).into(),
                decl: Some(NodeKey::from_decl(method)),
                is_pub: *is_pub,
            },
        );

        let self_param_node = params.first().filter(|p| is_self_param(p));
        let self_param_id = self_param_node.and_then(|p| {
            p.name.map(|pname| {
                self.define_at(
                    NodeKey::from_param(p),
                    DefInfo {
                        name: pname,
                        kind: DefKind::Param,
                        span: (p.span, method.source.src()).into(),
                        decl: None,
                        is_pub: false,
                    },
                )
            })
        });

        self.table.push(ScopeKind::InterfaceMethod {
            self_placeholder,
            self_param: self_param_id,
        });
        self.declare_generics(*generics, &method.source.src);

        for param in *params {
            self.resolve_type(param.ty);

            let Some(pname) = param.name else { continue };

            if is_self_param(param) {
                if let Some(id) = self_param_id {
                    self.table.declare_value(pname, id);
                }
                continue;
            }

            let def_id = self.define_at(
                NodeKey::from_param(param),
                DefInfo {
                    name: pname,
                    kind: DefKind::Param,
                    span: (param.span, method.source.src()).into(),
                    decl: None,
                    is_pub: false,
                },
            );
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

    fn declare_generics(
        &mut self,
        generics: Option<&[GenericType]>,
        current_src: &miette::NamedSource<Arc<String>>,
    ) {
        let Some(generics) = generics else { return };

        for generic in generics {
            let def_id = self.define_at(
                NodeKey::from_generic(generic),
                DefInfo {
                    name: generic.name.0,
                    kind: DefKind::GenericParam,
                    span: (generic.name.1, current_src.clone()).into(),
                    decl: None,
                    is_pub: false,
                },
            );

            self.table.declare_type(generic.name.0, def_id);

            if let Some(bounds) = generic.interfaces {
                for bound in bounds {
                    if self.table.lookup_type(bound.0).is_none() {
                        let bound_str = self.interner_resolve(&bound.0);

                        self.report(ResolveError::UnresolvedType {
                            name: bound_str,
                            src: current_src.clone(),
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
                    span: (stmt.span, self.named_src()).into(),
                    decl: None,
                    is_pub: false,
                });
                self.table.declare_value(name, def_id);

                self.result
                    .expr_bindings
                    .insert(NodeKey::from_stmt(stmt), Resolution::Def(def_id));
            }

            StatementKind::Assign { object, value } => {
                self.resolve_expr(object);
                self.resolve_expr(value);
            }

            StatementKind::CompoundAssign { object, value, .. } => {
                self.resolve_expr(object);
                self.resolve_expr(value);
            }

            StatementKind::Return { value } => {
                if let Some(value) = value {
                    self.resolve_expr(value);
                }
            }

            StatementKind::Break => {}
            StatementKind::Continue => {}

            StatementKind::While { condition, block } => {
                self.resolve_expr(condition);

                self.table.push(ScopeKind::Block);
                self.resolve_stmt(block);
                self.table.pop();
            }

            StatementKind::For {
                varname,
                iterator,
                block,
            } => {
                self.resolve_expr(iterator);

                self.table.push(ScopeKind::Block);

                let def_id = self.define(DefInfo {
                    name: varname.0,
                    kind: DefKind::Variable { is_const: false },
                    span: (varname.1, self.named_src()).into(),
                    decl: None,
                    is_pub: false,
                });

                self.result
                    .expr_bindings
                    .insert(NodeKey::from_stmt(stmt), Resolution::Def(def_id));

                self.table.declare_value(varname.0, def_id);

                self.resolve_stmt(block);
                self.table.pop();
            }

            StatementKind::FnDecl(decl) => {
                self.current_src = decl.source.src();

                let DeclarationKind::FnDecl { name, is_pub, .. } = &decl.kind else {
                    unreachable!("nested fn statement must be FnDecl")
                };

                let def_id = self.define_at(
                    NodeKey::from_decl(decl),
                    DefInfo {
                        name: name.0,
                        kind: DefKind::Function,
                        span: (name.1, decl.source.src()).into(),
                        decl: Some(NodeKey::from_decl(decl)),
                        is_pub: *is_pub,
                    },
                );

                self.table.declare_value(name.0, def_id);

                if let Some(parent) = self.current_fn_def {
                    self.result.nested_fn_parents.insert(def_id, parent);
                }

                let prev_fn = self.current_fn_def;
                self.current_fn_def = Some(def_id);

                // Nested functions may not capture the enclosing function's
                // params/locals/generics (no closures): hide them for the body.
                // Function definitions are not closure captures, so they stay
                // visible — a nested fn can recurse and call sibling fns.
                let capture_blocked: HashSet<DefId> = self
                    .table
                    .enclosing_defs()
                    .into_iter()
                    .filter(|def_id| {
                        !matches!(
                            self.result.defs.get(def_id).map(|info| &info.kind),
                            Some(DefKind::Function)
                        )
                    })
                    .collect();
                self.capture_stack.push(capture_blocked);
                self.resolve_fn_body(decl);
                self.capture_stack.pop();

                self.current_fn_def = prev_fn;
            }

            StatementKind::Expr(expr) => {
                self.resolve_expr(expr);
            }

            StatementKind::TrailingExpr(_) => panic!("that was not supposed to happen"),
        }
    }

    // --> Expressions

    fn resolve_expr(&mut self, expr: &'ctx Expression<'ctx>) {
        match expr.kind {
            ExpressionKind::Literal(_) => {}

            ExpressionKind::Ident { name, generic_args } => {
                let resolution = self.resolve_ident(name, expr.span);

                self.result
                    .expr_bindings
                    .insert(NodeKey::from_expr(expr), resolution);

                if let Some(args) = generic_args {
                    for arg in args {
                        self.resolve_type(arg);
                    }
                }
            }

            ExpressionKind::Binary { lhs, rhs, .. } => {
                self.resolve_expr(lhs);
                self.resolve_expr(rhs);
            }

            ExpressionKind::Unary { expr, .. } => {
                self.resolve_expr(expr);
            }

            ExpressionKind::Call { callee, args } => {
                self.resolve_expr(callee);

                for arg in args {
                    self.resolve_expr(arg);
                }
            }

            ExpressionKind::MacroCall { args, .. } => {
                for arg in args {
                    self.resolve_expr(arg);
                }
            }

            ExpressionKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.resolve_expr(condition);

                self.table.push(ScopeKind::Block);
                self.resolve_stmt(then_block);
                self.table.pop();

                if let Some(else_block) = else_block {
                    self.table.push(ScopeKind::Block);
                    self.resolve_stmt(else_block);
                    self.table.pop();
                }
            }

            ExpressionKind::Switch { object, .. } => {
                self.report(ResolveError::DisabledFeature {
                    reason: "not supported yet".into(),
                    src: self.named_src(),
                    span: object.span,
                });
            }

            ExpressionKind::FieldAccess { object, field } => {
                self.resolve_expr(object);

                // left for type checker, cuz NameResolver doesn't know any fields of objects
                let _ = field;
            }

            ExpressionKind::SliceAccess { object, index } => {
                self.resolve_expr(object);
                self.resolve_expr(index);
            }

            ExpressionKind::StructInit { ty, fields } => {
                self.resolve_expr(ty);

                if let Some(fields) = fields {
                    for field in fields {
                        // fields names left for type checker
                        self.resolve_expr(field.value);
                    }
                }
            }

            ExpressionKind::ArrayInit { elements } => {
                for elem in elements {
                    self.resolve_expr(elem);
                }
            }

            ExpressionKind::Block { stmts, trailing } => {
                self.table.push(ScopeKind::Block);

                for stmt in stmts {
                    self.resolve_stmt(stmt);
                }

                if let Some(expr) = trailing {
                    self.resolve_expr(expr);
                }

                self.table.pop();
            }

            ExpressionKind::Type(ty) => {
                self.resolve_type(ty);
            }
        }
    }

    fn resolve_ident(&mut self, name: Spur, span: SourceSpan) -> Resolution {
        if name == self.interner_intern("self") {
            return match self.table.enclosing_method_or_interface() {
                Some((_, Some(self_param))) => Resolution::SelfValue(self_param),
                _ => {
                    self.report(ResolveError::UnresolvedSelf {
                        src: self.named_src(),
                        span,
                    });

                    Resolution::Error
                }
            };
        }

        if name == self.interner_intern("Self") {
            return match self.table.enclosing_method_or_interface() {
                Some((self_def, _)) => Resolution::SelfType(self_def),
                _ => {
                    self.report(ResolveError::UnresolvedSelf {
                        src: self.named_src(),
                        span,
                    });

                    Resolution::Error
                }
            };
        }

        if let Some(def_id) = self.table.lookup_value(name) {
            if self.capture_stack.iter().any(|set| set.contains(&def_id)) {
                let captured = self.interner_resolve(&name);
                self.report(ResolveError::NestedFnCapture {
                    name: captured,
                    src: self.named_src(),
                    span,
                });
                return Resolution::Error;
            }

            self.check_visibility(def_id, &(self.current_src.clone(), span).into());
            return Resolution::Def(def_id);
        }

        if let Some(def_id) = self.table.lookup_type(name) {
            if self.capture_stack.iter().any(|set| set.contains(&def_id)) {
                let captured = self.interner_resolve(&name);
                self.report(ResolveError::NestedFnCapture {
                    name: captured,
                    src: self.named_src(),
                    span,
                });
                return Resolution::Error;
            }

            self.check_visibility(def_id, &(self.current_src.clone(), span).into());
            return Resolution::Def(def_id);
        }

        let name = self.interner_resolve(&name);

        self.report(ResolveError::UnresolvedIdent {
            name,
            src: self.named_src(),
            span,
        });

        Resolution::Error
    }

    // --> Types

    fn resolve_type(&mut self, ty: &'ctx TypeExpr<'ctx>) {
        match ty.kind {
            TypeKind::Builtin(_) | TypeKind::VaArgs => {}

            TypeKind::SelfType | TypeKind::SelfAlias => {
                let resolution = match self.table.enclosing_method_or_interface() {
                    Some((self_def, _)) => Resolution::SelfType(self_def),
                    None => {
                        self.errors.push(ResolveError::UnresolvedSelf {
                            src: self.named_src(),
                            span: ty.span,
                        });

                        Resolution::Error
                    }
                };

                self.result
                    .type_bindings
                    .insert(NodeKey(ty as *const _ as usize), resolution);
            }

            TypeKind::Named { name, generic_args } => {
                let resolution = match self.table.lookup_type(name) {
                    Some(def_id) => {
                        if self.capture_stack.iter().any(|set| set.contains(&def_id)) {
                            let captured = self.interner_resolve(&name);
                            self.errors.push(ResolveError::NestedFnCapture {
                                name: captured,
                                src: self.named_src(),
                                span: ty.span,
                            });

                            Resolution::Error
                        } else {
                            Resolution::Def(def_id)
                        }
                    }
                    None => {
                        let name = self.interner_resolve(&name);

                        self.errors.push(ResolveError::UnresolvedType {
                            name,
                            src: self.named_src(),
                            span: ty.span,
                        });

                        Resolution::Error
                    }
                };

                self.result
                    .type_bindings
                    .insert(NodeKey::from_type(ty), resolution);

                if let Some(args) = generic_args {
                    for arg in args {
                        self.resolve_type(arg);
                    }
                }
            }

            TypeKind::Const(inner)
            | TypeKind::SinglePointer(inner)
            | TypeKind::ManyPointer(inner) => {
                self.resolve_type(inner);
            }

            TypeKind::TypeOf(expr) => {
                self.resolve_expr(expr);
            }

            TypeKind::Array { element, len } => {
                self.resolve_type(element);

                if let Some(len) = len {
                    self.resolve_expr(len);
                }
            }

            TypeKind::Fn {
                params,
                generic_args,
                ret,
            } => {
                for param in params {
                    self.resolve_type(param);
                }

                // yeah, we really need this condition to correctly resolve return type
                if let Some(generics) = generic_args {
                    self.table.push(ScopeKind::Block);

                    self.declare_generics(Some(generics), &self.named_src());
                    self.resolve_type(ret);

                    self.table.pop();
                } else {
                    self.resolve_type(ret);
                }
            }
        }
    }
}
