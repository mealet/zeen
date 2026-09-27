use std::cell::RefCell;
use std::collections::HashSet;

use lasso::{Rodeo, Spur};
use miette::SourceSpan;

use zeen_ast::{Declaration, DeclarationKind};
use zeen_hir::{
    HirDecl, HirDeclKind, HirEnumVariantPayload, HirExpr, HirExprKind, HirFn, HirGenericParam,
    HirModule, HirPattern, HirPatternBinding, HirStmt, HirStmtKind, HirTypeExpr, HirTypeKind,
};
use zeen_resolve::{DefId, DefKind, ResolutionResult};

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
}

#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub occurrences: Vec<Occurrence>,
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
            occurrences: Vec::new(),
        };
        for decl in &module.decls {
            walker.walk_decl(decl);
        }
        walker
            .occurrences
            .sort_by_key(|item| (item.offset, item.len));
        Self {
            occurrences: walker.occurrences,
        }
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
    occurrences: Vec<Occurrence>,
}

impl<'ctx> Walker<'ctx> {
    fn push(
        &mut self,
        offset: usize,
        len: usize,
        role: Role,
        target: (Option<usize>, Option<usize>, Option<String>),
    ) {
        let offset = offset.min(self.text.len());
        let len = len.min(self.text.len().saturating_sub(offset));

        if len == 0 {
            return;
        }

        self.occurrences.push(Occurrence {
            offset,
            len,
            role,
            target_offset: target.0,
            target_len: target.1,
            target_file: target.2,
        });
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

    fn emit_def(&mut self, offset: usize, len: usize, role: Role) {
        self.push(offset, len, role, (None, None, None));
    }

    fn emit_target(&mut self, offset: usize, len: usize, role: Role, target: DefId) {
        self.push(offset, len, role, self.def_target_span(target));
    }

    fn emit_ref(&mut self, offset: usize, len: usize, target: DefId) {
        let Some(role) = self.role_of_def(target) else {
            return;
        };
        self.emit_target(offset, len, role, target);
    }

    fn emit_named(&mut self, span: SourceSpan, name: Spur, role: Role, target: Option<DefId>) {
        let (offset, len) = self
            .find_name(span, name)
            .unwrap_or((span.offset(), span.len()));

        let target_span = target
            .map(|def_id| self.def_target_span(def_id))
            .unwrap_or((None, None, None));

        self.push(offset, len, role, target_span);
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
                );

                self.walk_fn(func);
            }

            HirDeclKind::Struct(strukt) => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Type);

                self.emit_def(strukt.name.1.offset(), strukt.name.1.len(), role);

                for generic in &strukt.generics {
                    self.walk_generic(generic);
                }

                for field in &strukt.fields {
                    let role = self.role_of_def(field.def_id).unwrap_or(Role::Property);

                    if let Some((offset, len)) = self.find_field(decl.source.span, field.name) {
                        let target = self.def_target_span(field.def_id);
                        self.push(offset, len, role, target);
                    }

                    self.walk_type(&field.ty);
                }

                for method in &strukt.methods {
                    self.walk_decl(method);
                }
            }

            HirDeclKind::Interface(interface) => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Type);

                self.emit_def(interface.name.1.offset(), interface.name.1.len(), role);

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
                    self.walk_decl(method);
                }
            }

            HirDeclKind::Enum(enumeration) => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Type);

                self.emit_def(enumeration.name.1.offset(), enumeration.name.1.len(), role);

                for generic in &enumeration.generics {
                    self.walk_generic(generic);
                }

                for variant in &enumeration.variants {
                    self.emit_def(variant.span.offset(), variant.span.len(), Role::EnumMember);

                    match &variant.payload {
                        Some(HirEnumVariantPayload::Single(ty)) => self.walk_type(ty),
                        Some(HirEnumVariantPayload::Anonymous { .. }) | None => {}
                    }
                }

                for method in &enumeration.methods {
                    self.walk_decl(method);
                }
            }

            HirDeclKind::Alias(alias) => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Type);

                self.emit_def(alias.name.1.offset(), alias.name.1.len(), role);

                for generic in &alias.generics {
                    self.walk_generic(generic);
                }

                self.walk_type(&alias.ty);
            }

            HirDeclKind::ExternVar { name, ty, .. } => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Variable);

                self.emit_def(name.1.offset(), name.1.len(), role);
                self.walk_type(ty);
            }

            HirDeclKind::GlobalVar {
                name, ty, value, ..
            } => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Variable);

                self.emit_def(name.1.offset(), name.1.len(), role);
                self.walk_type(ty);
                self.walk_expr(value);
            }

            HirDeclKind::ExternLink | HirDeclKind::ExternInclude => {}
        }
    }

    fn walk_fn(&mut self, func: &HirFn) {
        for generic in &func.generics {
            self.walk_generic(generic);
        }

        for param in &func.params {
            if let Some(name) = param.name {
                let role = param
                    .def_id
                    .and_then(|def_id| self.role_of_def(def_id))
                    .unwrap_or(Role::Parameter);

                self.emit_named(param.span, name, role, param.def_id);
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
        self.emit_def(generic.name.1.offset(), generic.name.1.len(), Role::Type);
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

                self.emit_named(stmt.source.span, *name, role, Some(*def_id));

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

                self.emit_target(varname.1.offset(), varname.1.len(), role, *def_id);
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
            HirExprKind::VarRef(target)
            | HirExprKind::GenericParamRef(target)
            | HirExprKind::SelfValue(target) => {
                self.emit_ref(expr.source.span.offset(), expr.source.span.len(), *target);
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
                self.emit_def(field.1.offset(), field.1.len(), Role::Property);

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

                    self.emit_target(ty.2.offset(), ty.2.len(), role, def_id);
                }

                for arg in generic_args {
                    self.walk_type(arg);
                }

                for init in fields {
                    self.emit_def(init.span.offset(), init.span.len(), Role::Property);
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

            HirExprKind::Closure { def, .. } => self.walk_fn(def),

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
                self.emit_def(variant_span.offset(), variant_span.len(), Role::EnumMember);

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

        self.emit_target(
            binding.span.offset(),
            binding.span.len(),
            role,
            binding.def_id,
        );
    }

    fn walk_type(&mut self, ty: &HirTypeExpr) {
        match &ty.kind {
            HirTypeKind::Builtin(_) | HirTypeKind::VaArgs | HirTypeKind::Error => {}

            HirTypeKind::SelfType(target) | HirTypeKind::SelfAlias(target) => {
                self.emit_ref(ty.source.span.offset(), ty.source.span.len(), *target);
            }

            HirTypeKind::Named {
                def_id,
                generic_args,
            } => {
                self.emit_ref(ty.source.span.offset(), ty.source.span.len(), *def_id);

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

    Analysis { occurrences }
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
