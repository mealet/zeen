use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use lasso::{Rodeo, Spur};
use miette::SourceSpan;

use zeen_ast::{Declaration, DeclarationKind};
use zeen_hir::{
    HirDecl, HirDeclKind, HirEnumVariantPayload, HirExpr, HirExprKind, HirFn, HirGenericParam,
    HirId, HirModule, HirPattern, HirPatternBinding, HirStmt, HirStmtKind, HirTypeExpr,
    HirTypeKind,
};
use zeen_resolve::{DefId, DefKind, ResolutionResult};
use zeen_typecheck::result::TypeCheckResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Function,
    Method,
    Type,
    Variable,
    Parameter,
    Property,
    EnumMember,
}

impl Role {
    pub fn label(self) -> &'static str {
        match self {
            Role::Function => "function",
            Role::Method => "method",
            Role::Type => "type",
            Role::Variable => "variable",
            Role::Parameter => "parameter",
            Role::Property => "property",
            Role::EnumMember => "enum member",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub offset: usize,
    pub len: usize,
    pub role: Role,
    pub target_offset: Option<usize>,
    pub target_len: Option<usize>,
    pub target_file: Option<String>,
    pub hir: Option<HirId>,
    pub def: Option<DefId>,
    pub ty: Option<String>,
    pub type_hint: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub occurrences: Vec<Occurrence>,
    pub calls: Vec<CallSite>,
    pub field_hints: Vec<FieldHint>,
    pub members: HashMap<DefId, Vec<Member>>,
    pub(crate) fn_params: HashMap<DefId, Vec<Option<String>>>,
    pub(crate) raw_calls: Vec<RawCall>,
    pub(crate) raw_fields: Vec<RawFieldInit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub callee_offset: usize,
    pub callee_len: usize,
    pub callee: String,
    pub signature: String,
    pub args: Vec<(usize, usize)>,
    pub params: Vec<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberKind {
    Method,
    Field,
    Variant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub kind: MemberKind,
    pub def: DefId,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldHint {
    pub offset: usize,
    pub len: usize,
    pub ty: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawCall {
    pub call: HirId,
    pub callee: (usize, usize),
    pub callee_name: String,
    pub args: Vec<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawFieldInit {
    pub struct_def: DefId,
    pub name: Spur,
    pub offset: usize,
    pub len: usize,
}

impl Analysis {
    pub fn build(
        module: &HirModule,
        resolution: &ResolutionResult,
        interner: &RefCell<Rodeo>,
        text: &str,
        filename: &str,
        canonical: &str,
    ) -> Self {
        let interner = interner.borrow();

        let mut walker = Walker {
            resolution,
            interner,
            text,
            filename,
            canonical,
            methods: resolution.impls.values().flatten().copied().collect(),
            analysis: Analysis::default(),
        };

        for decl in &module.decls {
            walker.walk_decl(decl);
        }

        walker
            .analysis
            .occurrences
            .sort_by_key(|item| (item.offset, item.len));

        walker.analysis
    }

    pub fn at(&self, offset: usize) -> Option<&Occurrence> {
        let index = self
            .occurrences
            .partition_point(|item| item.offset <= offset)
            .checked_sub(1)?;

        let item = &self.occurrences[index];

        if offset < item.offset + item.len {
            Some(item)
        } else {
            None
        }
    }

    pub fn apply_types(
        &mut self,
        types: &TypeCheckResult,
        interner: Rc<RefCell<Rodeo>>,
        resolution: &ResolutionResult,
    ) {
        for occurrence in &mut self.occurrences {
            let type_id = occurrence
                .hir
                .and_then(|id| types.expr_types.get(&id).copied())
                .or_else(|| {
                    occurrence
                        .def
                        .and_then(|def| types.def_types.get(&def).copied())
                });

            if let Some(id) = type_id {
                occurrence.ty = Some(
                    types
                        .interner
                        .display_type(id, interner.clone(), resolution),
                );
            }
        }
    }

    pub fn resolve_calls(&mut self, types: &TypeCheckResult) {
        let mut calls = Vec::with_capacity(self.raw_calls.len());

        for raw in &self.raw_calls {
            let Some(resolution) = types.call_resolutions.get(&raw.call) else {
                continue;
            };

            let Some(params) = self.fn_params.get(&resolution.fn_def) else {
                continue;
            };

            calls.push(CallSite {
                callee_offset: raw.callee.0,
                callee_len: raw.callee.1,
                callee: raw.callee_name.clone(),
                signature: format!(
                    "{}({})",
                    raw.callee_name,
                    params
                        .iter()
                        .map(|param| param.as_deref().unwrap_or("_"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                args: raw.args.clone(),
                params: params.clone(),
            });
        }

        self.calls = calls;
    }

    pub fn resolve_fields(
        &mut self,
        types: &TypeCheckResult,
        interner: Rc<RefCell<Rodeo>>,
        resolution: &ResolutionResult,
    ) {
        let mut hints = Vec::with_capacity(self.raw_fields.len());

        for raw in &self.raw_fields {
            let Some(info) = types.struct_info.get(&raw.struct_def) else {
                continue;
            };

            let Some(field) = info.fields.iter().find(|field| field.name == raw.name) else {
                continue;
            };

            hints.push(FieldHint {
                offset: raw.offset,
                len: raw.len,
                ty: types
                    .interner
                    .display_type(field.field_ty, interner.clone(), resolution),
            });
        }

        self.field_hints = hints;
    }

    pub fn resolve_member_types(
        &mut self,
        types: &TypeCheckResult,
        interner: Rc<RefCell<Rodeo>>,
        resolution: &ResolutionResult,
    ) {
        for members in self.members.values_mut() {
            for member in members {
                let Some(id) = types.def_types.get(&member.def).copied() else {
                    continue;
                };

                member.signature = Some(types.interner.display_type(
                    id,
                    interner.clone(),
                    resolution,
                ));
            }
        }
    }
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

struct Walker<'ctx> {
    resolution: &'ctx ResolutionResult,
    interner: std::cell::Ref<'ctx, Rodeo>,
    text: &'ctx str,
    filename: &'ctx str,
    canonical: &'ctx str,
    methods: HashSet<DefId>,
    analysis: Analysis,
}

impl<'ctx> Walker<'ctx> {
    fn push(
        &mut self,
        offset: usize,
        len: usize,
        role: Role,
        target: (Option<usize>, Option<usize>, Option<String>),
        hir: Option<HirId>,
        def: Option<DefId>,
    ) -> Option<(usize, usize)> {
        let offset = offset.min(self.text.len());
        let len = len.min(self.text.len().saturating_sub(offset));

        if len == 0 {
            return None;
        }

        self.analysis.occurrences.push(Occurrence {
            offset,
            len,
            role,
            target_offset: target.0,
            target_len: target.1,
            target_file: target.2,
            hir,
            def,
            ty: None,
            type_hint: false,
        });

        Some((offset, len))
    }

    fn role_of_kind(&self, kind: &DefKind, def_id: DefId) -> Role {
        match kind {
            DefKind::Function => {
                if self.methods.contains(&def_id) {
                    Role::Method
                } else {
                    Role::Function
                }
            }

            DefKind::Struct
            | DefKind::Interface
            | DefKind::TypeAlias
            | DefKind::Enum
            | DefKind::GenericParam
            | DefKind::InterfaceSelfPlaceholder => Role::Type,

            DefKind::EnumVariant => Role::EnumMember,

            DefKind::Variable { .. } | DefKind::GlobalVar { .. } | DefKind::ExternVar => {
                Role::Variable
            }

            DefKind::Param => Role::Parameter,
            DefKind::Field => Role::Property,
        }
    }

    fn role_of_def(&self, def_id: DefId) -> Option<Role> {
        let info = self.resolution.defs.get(&def_id)?;

        Some(self.role_of_kind(&info.kind, def_id))
    }

    fn file_matches(&self, name: &str) -> bool {
        name == self.filename || name == self.canonical
    }

    fn def_target_span(&self, def_id: DefId) -> (Option<usize>, Option<usize>, Option<String>) {
        let Some(info) = self.resolution.defs.get(&def_id) else {
            return (None, None, None);
        };

        let name = info.span.src.name();

        if name == self.filename || name == self.canonical {
            return (
                Some(info.span.span.offset()),
                Some(info.span.span.len()),
                None,
            );
        }

        (
            Some(info.span.span.offset()),
            Some(info.span.span.len()),
            Some(name.to_string()),
        )
    }

    fn emit_def(
        &mut self,
        offset: usize,
        len: usize,
        role: Role,
        def: Option<DefId>,
    ) -> Option<(usize, usize)> {
        self.push(offset, len, role, (None, None, None), None, def)
    }

    fn emit_target(
        &mut self,
        offset: usize,
        len: usize,
        role: Role,
        target: DefId,
        hir: Option<HirId>,
    ) -> Option<(usize, usize)> {
        self.push(
            offset,
            len,
            role,
            self.def_target_span(target),
            hir,
            Some(target),
        )
    }

    fn emit_ref(
        &mut self,
        offset: usize,
        len: usize,
        target: DefId,
        hir: Option<HirId>,
    ) -> Option<(usize, usize)> {
        let Some(role) = self.role_of_def(target) else {
            return None;
        };
        self.emit_target(offset, len, role, target, hir)
    }

    fn emit_named(
        &mut self,
        span: SourceSpan,
        name: Spur,
        role: Role,
        target: Option<DefId>,
        hir: Option<HirId>,
    ) -> Option<(usize, usize)> {
        let (offset, len) = self
            .find_name(span, name)
            .unwrap_or((span.offset(), span.len()));

        let target_span = target
            .map(|def_id| self.def_target_span(def_id))
            .unwrap_or((None, None, None));

        self.push(offset, len, role, target_span, hir, target)
    }

    fn scope_text(&self, span: SourceSpan) -> Option<(&str, usize)> {
        let start = span.offset().min(self.text.len());
        let end = start.saturating_add(span.len()).min(self.text.len());

        self.text.get(start..end).map(|haystack| (haystack, start))
    }

    fn find_name(&self, scope: SourceSpan, name: Spur) -> Option<(usize, usize)> {
        let (haystack, start) = self.scope_text(scope)?;
        let needle = self.interner.resolve(&name);

        let mut search_from = 0;

        while let Some(position) = haystack[search_from..].find(needle) {
            let absolute = search_from + position;

            let before_ok = absolute == 0 || !is_ident_byte(haystack.as_bytes()[absolute - 1]);

            let after = absolute + needle.len();
            let after_ok = after >= haystack.len() || !is_ident_byte(haystack.as_bytes()[after]);

            if before_ok && after_ok {
                return Some((start + absolute, needle.len()));
            }

            search_from = absolute + 1;
        }

        None
    }

    fn find_field(&self, scope: SourceSpan, name: Spur) -> Option<(usize, usize)> {
        let needle = format!("{}:", self.interner.resolve(&name));
        let (haystack, start) = self.scope_text(scope)?;

        let mut search_from = 0;

        while let Some(position) = haystack[search_from..].find(needle.as_str()) {
            let absolute = search_from + position;

            let before_ok = absolute == 0 || !is_ident_byte(haystack.as_bytes()[absolute - 1]);

            if before_ok {
                return Some((start + absolute, needle.len() - 1));
            }

            search_from = absolute + 1;
        }

        None
    }

    fn fn_role(&self, def_id: DefId) -> Role {
        if self.methods.contains(&def_id) {
            Role::Method
        } else {
            Role::Function
        }
    }

    fn collect_method(&mut self, owner: DefId, method: &HirDecl) {
        let HirDeclKind::Fn(func) = &method.kind else {
            return;
        };

        let start = func.name.1.offset().min(self.text.len());
        let end = start.saturating_add(func.name.1.len()).min(self.text.len());

        let Some(name) = self.text.get(start..end) else {
            return;
        };

        self.analysis
            .members
            .entry(owner)
            .or_default()
            .push(Member {
                name: name.to_string(),
                kind: MemberKind::Method,
                def: method.def_id,
                signature: None,
            });
    }

    fn walk_decl(&mut self, decl: &HirDecl) {
        if !self.file_matches(decl.source.src.name()) {
            return;
        }

        match &decl.kind {
            HirDeclKind::Fn(func) => {
                self.emit_def(
                    func.name.1.offset(),
                    func.name.1.len(),
                    self.fn_role(decl.def_id),
                    Some(decl.def_id),
                );

                self.walk_fn(func, decl.def_id);
            }

            HirDeclKind::Struct(strukt) => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Type);

                self.emit_def(
                    strukt.name.1.offset(),
                    strukt.name.1.len(),
                    role,
                    Some(decl.def_id),
                );

                for generic in &strukt.generics {
                    self.walk_generic(generic);
                }

                for field in &strukt.fields {
                    let role = self.role_of_def(field.def_id).unwrap_or(Role::Property);

                    if let Some((offset, len)) = self.find_field(decl.source.span, field.name) {
                        let target = self.def_target_span(field.def_id);
                        self.push(offset, len, role, target, None, Some(field.def_id));
                    }

                    self.analysis
                        .members
                        .entry(decl.def_id)
                        .or_default()
                        .push(Member {
                            name: self.interner.resolve(&field.name).to_string(),
                            kind: MemberKind::Field,
                            def: field.def_id,
                            signature: None,
                        });

                    self.walk_type(&field.ty);
                }

                for method in &strukt.methods {
                    self.collect_method(decl.def_id, method);
                    self.walk_decl(method);
                }
            }

            HirDeclKind::Interface(interface) => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Type);

                self.emit_def(
                    interface.name.1.offset(),
                    interface.name.1.len(),
                    role,
                    Some(decl.def_id),
                );

                for generic in &interface.generics {
                    self.walk_generic(generic);
                }

                for method in &interface.methods {
                    self.walk_decl(method);
                }
            }

            HirDeclKind::Implement(implement) => {
                for generic in &implement.generics {
                    self.walk_generic(generic);
                }

                for ty in &implement.object_generic_types {
                    self.walk_type(ty);
                }

                for method in &implement.methods {
                    if let Some(owner) = implement.object {
                        self.collect_method(owner, method);
                    }

                    self.walk_decl(method);
                }
            }

            HirDeclKind::Enum(enumeration) => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Type);

                self.emit_def(
                    enumeration.name.1.offset(),
                    enumeration.name.1.len(),
                    role,
                    Some(decl.def_id),
                );

                for generic in &enumeration.generics {
                    self.walk_generic(generic);
                }

                for variant in &enumeration.variants {
                    self.emit_def(
                        variant.span.offset(),
                        variant.span.len(),
                        Role::EnumMember,
                        Some(variant.def_id),
                    );

                    self.analysis
                        .members
                        .entry(decl.def_id)
                        .or_default()
                        .push(Member {
                            name: self.interner.resolve(&variant.name).to_string(),
                            kind: MemberKind::Variant,
                            def: variant.def_id,
                            signature: None,
                        });

                    match &variant.payload {
                        Some(HirEnumVariantPayload::Single(ty)) => self.walk_type(ty),
                        Some(HirEnumVariantPayload::Anonymous { .. }) | None => {}
                    }
                }

                for method in &enumeration.methods {
                    self.collect_method(decl.def_id, method);
                    self.walk_decl(method);
                }
            }

            HirDeclKind::Alias(alias) => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Type);

                self.emit_def(
                    alias.name.1.offset(),
                    alias.name.1.len(),
                    role,
                    Some(decl.def_id),
                );

                for generic in &alias.generics {
                    self.walk_generic(generic);
                }

                self.walk_type(&alias.ty);
            }

            HirDeclKind::ExternVar { name, ty, .. } => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Variable);

                self.emit_def(name.1.offset(), name.1.len(), role, Some(decl.def_id));
                self.walk_type(ty);
            }

            HirDeclKind::GlobalVar {
                name, ty, value, ..
            } => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Variable);

                self.emit_def(name.1.offset(), name.1.len(), role, Some(decl.def_id));
                self.walk_type(ty);
                self.walk_expr(value);
            }

            HirDeclKind::ExternLink | HirDeclKind::ExternInclude => {}
        }
    }

    fn walk_fn(&mut self, func: &HirFn, def_id: DefId) {
        let mut params = Vec::with_capacity(func.params.len());

        for param in &func.params {
            params.push(
                param
                    .name
                    .map(|name| self.interner.resolve(&name).to_string()),
            );
        }

        self.analysis.fn_params.insert(def_id, params);

        for generic in &func.generics {
            self.walk_generic(generic);
        }

        for param in &func.params {
            if let Some(name) = param.name {
                let role = param
                    .def_id
                    .and_then(|def_id| self.role_of_def(def_id))
                    .unwrap_or(Role::Parameter);

                self.emit_named(param.span, name, role, param.def_id, None);
            }

            self.walk_type(&param.ty);
        }

        if let Some(ret) = &func.return_type {
            self.walk_type(ret);
        }

        if let Some(body) = &func.body {
            self.walk_stmt(body);
        }
    }

    fn walk_generic(&mut self, generic: &HirGenericParam) {
        self.emit_def(
            generic.name.1.offset(),
            generic.name.1.len(),
            Role::Type,
            Some(generic.def_id),
        );
    }

    fn walk_stmt(&mut self, stmt: &HirStmt) {
        match &stmt.kind {
            HirStmtKind::Let {
                def_id,
                name,
                explicit_type,
                value,
                ..
            } => {
                let role = self.role_of_def(*def_id).unwrap_or(Role::Variable);

                if self
                    .emit_named(stmt.source.span, *name, role, Some(*def_id), None)
                    .is_some()
                    && explicit_type.is_none()
                {
                    if let Some(last) = self.analysis.occurrences.last_mut() {
                        last.type_hint = true;
                    }
                }

                if let Some(ty) = explicit_type {
                    self.walk_type(ty);
                }

                if let Some(value) = value {
                    self.walk_expr(value);
                }
            }

            HirStmtKind::Assign { object, value }
            | HirStmtKind::CompoundAssign { object, value, .. } => {
                self.walk_expr(object);
                self.walk_expr(value);
            }

            HirStmtKind::Return { value } => {
                if let Some(value) = value {
                    self.walk_expr(value);
                }
            }

            HirStmtKind::Break | HirStmtKind::Continue | HirStmtKind::Error => {}

            HirStmtKind::While { condition, block } => {
                self.walk_expr(condition);
                self.walk_stmt(block);
            }

            HirStmtKind::For {
                def_id,
                varname,
                iterator,
                block,
                ..
            } => {
                let role = self.role_of_def(*def_id).unwrap_or(Role::Variable);

                if self
                    .emit_target(varname.1.offset(), varname.1.len(), role, *def_id, None)
                    .is_some()
                && let Some(last) = self.analysis.occurrences.last_mut() {
                        last.type_hint = true;
                }

                self.walk_expr(iterator);
                self.walk_stmt(block);
            }

            HirStmtKind::Expr(expr) => self.walk_expr(expr),

            HirStmtKind::FnDecl(decl) => self.walk_decl(decl),
        }
    }

    fn walk_expr(&mut self, expr: &HirExpr) {
        match &expr.kind {
            HirExprKind::Literal(_) | HirExprKind::Error => {}
            HirExprKind::VarRef { def: target, generic_args: _ }
            | HirExprKind::GenericParamRef(target)
            | HirExprKind::SelfValue(target) => {
                self.emit_ref(
                    expr.source.span.offset(),
                    expr.source.span.len(),
                    *target,
                    Some(expr.id),
                );
            }

            HirExprKind::Binary { lhs, rhs, .. } => {
                self.walk_expr(lhs);
                self.walk_expr(rhs);
            }

            HirExprKind::Unary { expr: inner, .. } => self.walk_expr(inner),

            HirExprKind::Call {
                callee,
                args,
                generic_args,
                ..
            } => {
                self.analysis.raw_calls.push(RawCall {
                    call: expr.id,
                    callee: (callee.source.span.offset(), callee.source.span.len()),
                    callee_name: self
                        .text
                        .get(
                            callee.source.span.offset()
                                ..callee.source.span.offset() + callee.source.span.len(),
                        )
                        .unwrap_or("")
                        .to_string(),
                    args: args
                        .iter()
                        .map(|arg| (arg.source.span.offset(), arg.source.span.len()))
                        .collect(),
                });

                self.walk_expr(callee);

                for arg in args {
                    self.walk_expr(arg);
                }

                for ty in generic_args {
                    self.walk_type(ty);
                }
            }

            HirExprKind::MacroCall { args, .. } => {
                for arg in args {
                    self.walk_expr(arg);
                }
            }

            HirExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.walk_expr(condition);
                self.walk_stmt(then_block);

                if let Some(else_block) = else_block {
                    self.walk_stmt(else_block);
                }
            }

            HirExprKind::Switch { object, arms } => {
                self.walk_expr(object);

                for arm in arms {
                    self.walk_pattern(&arm.pattern);

                    if let Some(guard) = &arm.guard {
                        self.walk_expr(guard);
                    }

                    self.walk_expr(&arm.body);
                }
            }

            HirExprKind::FieldAccess {
                object,
                field,
                object_generic_args,
            } => {
                self.walk_expr(object);
                self.emit_def(field.1.offset(), field.1.len(), Role::Property, None);

                for ty in object_generic_args {
                    self.walk_type(ty);
                }
            }

            HirExprKind::SliceAccess { object, index } => {
                self.walk_expr(object);
                self.walk_expr(index);
            }

            HirExprKind::StructInit {
                ty,
                generic_args,
                fields,
            } => {
                if let Some(def_id) = ty.0 {
                    let role = self.role_of_def(def_id).unwrap_or(Role::Type);

                    self.emit_target(ty.2.offset(), ty.2.len(), role, def_id, None);
                }

                for arg in generic_args {
                    self.walk_type(arg);
                }

                for init in fields {
                    self.emit_def(init.span.offset(), init.span.len(), Role::Property, None);

                    if let Some(def_id) = ty.0 {
                        self.analysis.raw_fields.push(RawFieldInit {
                            struct_def: def_id,
                            name: init.name,
                            offset: init.span.offset(),
                            len: init.span.len(),
                        });
                    }

                    self.walk_expr(&init.value);
                }
            }

            HirExprKind::ArrayInit { elements } => {
                for element in elements {
                    self.walk_expr(element);
                }
            }

            HirExprKind::ArrayRepeatInit { element, len } => {
                self.walk_expr(element);
                self.walk_expr(len);
            }

            HirExprKind::Block { stmts, trailing } => {
                for stmt in stmts {
                    self.walk_stmt(stmt);
                }

                if let Some(trailing) = trailing {
                    self.walk_expr(trailing);
                }
            }

            HirExprKind::Type(ty) => self.walk_type(ty),

            HirExprKind::Closure { def_id, def, .. } => self.walk_fn(def, *def_id),

            HirExprKind::Range { start, end, .. } => {
                if let Some(start) = start {
                    self.walk_expr(start);
                }

                if let Some(end) = end {
                    self.walk_expr(end);
                }
            }
        }
    }

    fn walk_pattern(&mut self, pattern: &HirPattern) {
        match pattern {
            HirPattern::Literal(_) | HirPattern::Wildcard | HirPattern::Range { .. } => {}

            HirPattern::Binding(binding) => self.walk_binding(binding),

            HirPattern::Enum {
                variant_span,
                binding,
                ..
            } => {
                self.emit_def(
                    variant_span.offset(),
                    variant_span.len(),
                    Role::EnumMember,
                    None,
                );

                if let Some(binding) = binding {
                    self.walk_binding(binding);
                }
            }

            HirPattern::Or(patterns) => {
                for pattern in patterns {
                    self.walk_pattern(pattern);
                }
            }
        }
    }

    fn walk_binding(&mut self, binding: &HirPatternBinding) {
        let role = self.role_of_def(binding.def_id).unwrap_or(Role::Variable);

        if self
            .emit_target(
                binding.span.offset(),
                binding.span.len(),
                role,
                binding.def_id,
                None,
            )
            .is_some()
        {
            if let Some(last) = self.analysis.occurrences.last_mut() {
                last.type_hint = true;
            }
        }
    }

    fn walk_type(&mut self, ty: &HirTypeExpr) {
        match &ty.kind {
            HirTypeKind::Builtin(_) | HirTypeKind::VaArgs | HirTypeKind::Error => {}

            HirTypeKind::SelfType(target) | HirTypeKind::SelfAlias(target) => {
                self.emit_ref(
                    ty.source.span.offset(),
                    ty.source.span.len(),
                    *target,
                    Some(ty.id),
                );
            }

            HirTypeKind::Named {
                def_id,
                generic_args,
            } => {
                self.emit_ref(
                    ty.source.span.offset(),
                    ty.source.span.len(),
                    *def_id,
                    Some(ty.id),
                );

                for arg in generic_args {
                    self.walk_type(arg);
                }
            }

            HirTypeKind::Const(inner)
            | HirTypeKind::SinglePointer(inner)
            | HirTypeKind::ManyPointer(inner) => self.walk_type(inner),

            HirTypeKind::TypeOf(expr) => self.walk_expr(expr),

            HirTypeKind::Array { element, len } => {
                self.walk_type(element);

                if let Some(len) = len {
                    self.walk_expr(len);
                }
            }

            HirTypeKind::Fn {
                params,
                generics,
                ret,
            } => {
                for param in params {
                    self.walk_type(param);
                }

                for generic in generics {
                    self.walk_generic(generic);
                }

                self.walk_type(ret);
            }

            HirTypeKind::FatFn { params, ret, .. } => {
                for param in params {
                    self.walk_type(param);
                }

                self.walk_type(ret);
            }
        }
    }
}

pub fn build_syntax_fallback(program: &[&Declaration<'_>]) -> Analysis {
    let mut occurrences = Vec::new();

    for decl in program {
        walk_syntax_decl(decl, false, &mut occurrences);
    }

    occurrences.sort_by_key(|item| (item.offset, item.len));

    Analysis {
        occurrences,
        ..Default::default()
    }
}

fn push_syntax(occurrences: &mut Vec<Occurrence>, offset: usize, len: usize, role: Role) {
    if len == 0 {
        return;
    }

    occurrences.push(Occurrence {
        offset,
        len,
        role,
        target_offset: None,
        target_len: None,
        target_file: None,
        hir: None,
        def: None,
        ty: None,
        type_hint: false,
    });
}

fn walk_syntax_decl(decl: &Declaration<'_>, method: bool, occurrences: &mut Vec<Occurrence>) {
    match &decl.kind {
        DeclarationKind::FnDecl { name, .. } => {
            push_syntax(
                occurrences,
                name.1.offset(),
                name.1.len(),
                if method { Role::Method } else { Role::Function },
            );
        }

        DeclarationKind::StructDecl { name, methods, .. }
        | DeclarationKind::InterfaceDecl { name, methods, .. } => {
            push_syntax(occurrences, name.1.offset(), name.1.len(), Role::Type);

            for method in *methods {
                walk_syntax_decl(method, true, occurrences);
            }
        }

        DeclarationKind::EnumDecl {
            name,
            variants,
            methods,
            ..
        } => {
            push_syntax(occurrences, name.1.offset(), name.1.len(), Role::Type);

            for variant in *variants {
                push_syntax(
                    occurrences,
                    variant.span.offset(),
                    variant.span.len(),
                    Role::EnumMember,
                );
            }

            for method in *methods {
                walk_syntax_decl(method, true, occurrences);
            }
        }

        DeclarationKind::ImplementDecl { methods, .. } => {
            for method in *methods {
                walk_syntax_decl(method, true, occurrences);
            }
        }

        DeclarationKind::ExternVar { name, .. } | DeclarationKind::GlobalVar { name, .. } => {
            push_syntax(occurrences, name.1.offset(), name.1.len(), Role::Variable);
        }

        DeclarationKind::Alias(alias) => {
            push_syntax(
                occurrences,
                alias.name.1.offset(),
                alias.name.1.len(),
                Role::Type,
            );
        }

        DeclarationKind::Use { .. }
        | DeclarationKind::ExternLink { .. }
        | DeclarationKind::ExternInclude { .. }
        | DeclarationKind::ConditionalBlock(_) => {}
    }
}
