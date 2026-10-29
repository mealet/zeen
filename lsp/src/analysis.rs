use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use lasso::{Rodeo, Spur};
use miette::SourceSpan;

use zeen_ast::expressions::{Expression, ExpressionKind, Pattern};
use zeen_ast::statements::{Statement, StatementKind};
use zeen_ast::{Declaration, DeclarationKind};
use zeen_hir::{
    HirDecl, HirDeclKind, HirEnumVariantPayload, HirExpr, HirExprKind, HirFn, HirGenericParam,
    HirId, HirModule, HirPattern, HirPatternBinding, HirStmt, HirStmtKind, HirTypeExpr,
    HirTypeKind,
};
use zeen_resolve::{DefId, DefKind, ResolutionResult};
use zeen_typecheck::result::TypeCheckResult;
use zeen_types::{Type, TypeId};

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
    pub target_module: Option<String>,
    pub hir: Option<HirId>,
    pub def: Option<DefId>,
    pub owner: Option<DefId>,
    pub type_def: Option<DefId>,
    pub ty: Option<String>,
    pub caps: Option<String>,
    pub is_pub: Option<bool>,
    pub is_const: Option<bool>,
    pub type_hint: bool,
    pub scope: Option<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub start: usize,
    pub end: usize,
    pub owner: DefId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawAccess {
    pub access: HirId,
    pub field: (usize, usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldTarget {
    pub field_def: DefId,
    pub ty: String,
    pub is_pub: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Analysis {
    pub occurrences: Vec<Occurrence>,
    pub calls: Vec<CallSite>,
    pub field_hints: Vec<FieldHint>,
    pub members: HashMap<DefId, Vec<Member>>,
    pub member_owner: HashMap<DefId, DefId>,
    pub type_kinds: HashMap<DefId, TypeKind>,
    pub variant_fields: HashMap<DefId, Vec<String>>,
    pub scopes: Vec<Scope>,
    pub full_spans: HashMap<DefId, (usize, usize)>,
    pub access_types: HashMap<(usize, usize), String>,
    pub call_types: HashMap<(usize, usize), String>,
    pub call_generics: HashMap<(usize, usize), String>,
    pub method_calls: HashMap<(usize, usize), DefId>,
    pub field_targets: HashMap<(usize, usize), FieldTarget>,
    pub fn_generics: HashMap<DefId, Vec<String>>,
    pub method_self: HashMap<DefId, String>,
    pub fn_signatures: HashMap<DefId, String>,
    pub fn_param_labels: HashMap<DefId, Vec<String>>,
    pub fn_rets: HashMap<DefId, String>,
    pub fn_source_sig: HashMap<DefId, String>,
    pub def_spans: HashMap<DefId, (usize, usize, Option<String>)>,
    pub enum_payloads: HashMap<String, Vec<(String, Option<String>)>>,
    pub(crate) fn_params: HashMap<DefId, Vec<Option<String>>>,
    pub(crate) raw_calls: Vec<RawCall>,
    pub(crate) raw_fields: Vec<RawFieldInit>,
    pub(crate) raw_access: Vec<RawAccess>,
    pub implements: Vec<(DefId, DefId)>,
    pub interfaces: HashMap<String, Vec<Member>>,
    pub generic_bounds: HashMap<DefId, Vec<DefId>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallSite {
    pub callee_offset: usize,
    pub callee_len: usize,
    pub callee: String,
    pub signature: String,
    pub args: Vec<(usize, usize)>,
    pub params: Vec<Option<String>>,
    pub param_types: Vec<Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureData {
    pub label: String,
    pub params: Vec<String>,
    pub active: Option<u32>,
}

fn short_callee_name(callee: &str) -> &str {
    let base = callee.rsplit('.').next().unwrap_or(callee);

    match base.find(['#', '[', '(']) {
        Some(index) => &base[..index],
        None => base,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberKind {
    Method,
    Field,
    Variant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    Struct,
    Enum,
    Interface,
    Alias,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub kind: MemberKind,
    pub def: DefId,
    pub signature: Option<String>,
    pub is_static: bool,
    pub is_pub: bool,
    pub is_interface: bool,
    pub arity: Option<usize>,
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
    pub field: Option<(usize, usize)>,
    pub explicit_generics: bool,
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

        let mut methods: HashSet<DefId> = resolution.impls.values().flatten().copied().collect();

        for decl in &module.decls {
            if let HirDeclKind::Implement(implement) = &decl.kind
                && implement.interface.is_none()
            {
                methods.extend(implement.methods.iter().map(|method| method.def_id));
            }
        }

        let mut walker = Walker {
            resolution,
            interner,
            text,
            filename,
            canonical,
            methods,
            analysis: Analysis::default(),
            block_ends: Vec::new(),
            local_scopes: HashMap::new(),
        };

        for decl in &module.decls {
            walker.walk_cross_file(decl);
        }

        for decl in &module.decls {
            walker.walk_decl(decl);
        }

        for occurrence in &mut walker.analysis.occurrences {
            let Some(def) = occurrence.def else {
                continue;
            };

            let Some(info) = resolution.defs.get(&def) else {
                continue;
            };

            occurrence.is_pub = Some(info.is_pub);
            occurrence.owner = walker.analysis.member_owner.get(&def).copied();
            occurrence.is_const = match info.kind {
                DefKind::Variable { is_const } | DefKind::GlobalVar { is_const } => Some(is_const),
                _ => None,
            };
            occurrence.target_module = resolution.def_modules.get(&def).cloned();

            record_def_span(
                &mut walker.analysis.def_spans,
                resolution,
                def,
                walker.filename,
                walker.canonical,
            );
        }

        for def in walker
            .analysis
            .members
            .keys()
            .copied()
            .chain(
                walker
                    .analysis
                    .members
                    .values()
                    .flatten()
                    .map(|member| member.def),
            )
            .collect::<HashSet<_>>()
        {
            if walker.analysis.def_spans.contains_key(&def) {
                continue;
            }

            record_def_span(
                &mut walker.analysis.def_spans,
                resolution,
                def,
                walker.filename,
                walker.canonical,
            );
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

    pub fn signature_at(&self, offset: usize) -> Option<SignatureData> {
        for call in &self.calls {
            let end = call
                .args
                .last()
                .map(|(start, len)| start + len)
                .unwrap_or(call.callee_offset + call.callee_len);

            if offset < call.callee_offset || offset > end {
                continue;
            }

            let takes_self = self
                .method_calls
                .get(&(call.callee_offset, call.callee_len))
                .is_some_and(|def| self.method_self.contains_key(def));
            let skip = usize::from(takes_self);

            let fn_labels = self
                .method_calls
                .get(&(call.callee_offset, call.callee_len))
                .and_then(|def| {
                    self.fn_param_labels
                        .get(def)
                        .map(|labels| (labels, self.fn_rets.get(def)))
                });

            let (params, label) = match fn_labels {
                Some((labels, ret)) => {
                    let params: Vec<String> = labels.iter().skip(skip).cloned().collect();
                    let label = match ret {
                        Some(ret) => format!(
                            "{}({}) {ret}",
                            short_callee_name(&call.callee),
                            params.join(", ")
                        ),
                        None => {
                            format!("{}({})", short_callee_name(&call.callee), params.join(", "))
                        }
                    };

                    (params, label)
                }
                None => {
                    let params: Vec<String> = call
                        .params
                        .iter()
                        .skip(skip)
                        .map(|param| param.as_deref().unwrap_or("_").to_string())
                        .collect();

                    (
                        params.clone(),
                        format!("{}({})", short_callee_name(&call.callee), params.join(", ")),
                    )
                }
            };

            let active = if params.is_empty() {
                None
            } else {
                call.args
                    .iter()
                    .position(|(start, len)| offset >= *start && offset <= start + len)
                    .map(|index| index.min(params.len() - 1) as u32)
            };

            return Some(SignatureData {
                label,
                params,
                active,
            });
        }

        None
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
                occurrence.caps = Some(caps_display(types, id));

                match types.interner.get(id) {
                    Type::Struct { def_id, .. } | Type::Enum { def_id, .. } => {
                        occurrence.type_def = Some(*def_id);
                    }
                    _ => {}
                }
            }
        }
    }

    pub fn resolve_calls(
        &mut self,
        types: &TypeCheckResult,
        interner: Rc<RefCell<Rodeo>>,
        resolution: &ResolutionResult,
    ) {
        let mut calls = Vec::with_capacity(self.raw_calls.len());

        for raw in &self.raw_calls {
            let Some(call_res) = types.call_resolutions.get(&raw.call) else {
                continue;
            };

            self.method_calls.insert(raw.callee, call_res.fn_def);

            let Some(params) = self.fn_params.get(&call_res.fn_def) else {
                continue;
            };

            let skip = usize::from(self.method_self.contains_key(&call_res.fn_def));
            let param_types = match types.def_types.get(&call_res.fn_def) {
                Some(id) => match types.interner.get(*id) {
                    Type::Fn { params, .. } => params
                        .iter()
                        .skip(skip)
                        .map(|param| {
                            Some(
                                types
                                    .interner
                                    .display_type(*param, interner.clone(), resolution),
                            )
                        })
                        .collect(),
                    _ => Vec::new(),
                },
                None => Vec::new(),
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
                param_types,
            });
        }

        self.calls = calls;

        for raw in &self.raw_calls {
            let Some(id) = types.expr_types.get(&raw.call).copied() else {
                continue;
            };

            self.call_types.insert(
                raw.callee,
                types
                    .interner
                    .display_type(id, interner.clone(), resolution),
            );
        }

        for raw in &self.raw_calls {
            if raw.explicit_generics {
                continue;
            }

            let Some(resolved) = types.call_resolutions.get(&raw.call) else {
                continue;
            };

            if resolved.generic_args.is_empty() {
                continue;
            }

            let rendered: Vec<String> = resolved
                .generic_args
                .iter()
                .map(|id| {
                    types
                        .interner
                        .display_type(*id, interner.clone(), resolution)
                })
                .collect();

            self.call_generics
                .insert(raw.callee, format!("[{}]", rendered.join(", ")));
        }

        for raw in &self.raw_access {
            let Some(id) = types.expr_types.get(&raw.access).copied() else {
                continue;
            };

            self.access_types.insert(
                raw.field,
                types
                    .interner
                    .display_type(id, interner.clone(), resolution),
            );
        }
    }

    pub fn resolve_field_targets(
        &mut self,
        types: &TypeCheckResult,
        interner: Rc<RefCell<Rodeo>>,
        resolution: &ResolutionResult,
    ) {
        for raw in &self.raw_access {
            let Some(field_def) = types.field_resolutions.get(&raw.access).copied() else {
                continue;
            };

            let Some(id) = types.expr_types.get(&raw.access).copied() else {
                continue;
            };

            let is_pub = resolution
                .defs
                .get(&field_def)
                .is_some_and(|info| info.is_pub);

            self.field_targets.insert(
                raw.field,
                FieldTarget {
                    field_def,
                    ty: types
                        .interner
                        .display_type(id, interner.clone(), resolution),
                    is_pub,
                },
            );
        }

        for raw in &self.raw_fields {
            let Some(info) = types.struct_info.get(&raw.struct_def) else {
                continue;
            };

            let Some(field) = info.fields.iter().find(|field| field.name == raw.name) else {
                continue;
            };

            self.field_targets.insert(
                (raw.offset, raw.len),
                FieldTarget {
                    field_def: field.field_def,
                    ty: types
                        .interner
                        .display_type(field.field_ty, interner.clone(), resolution),
                    is_pub: field.is_pub,
                },
            );
        }
    }

    pub fn resolve_fn_signatures(
        &mut self,
        types: &TypeCheckResult,
        interner: Rc<RefCell<Rodeo>>,
        resolution: &ResolutionResult,
    ) {
        let defs: Vec<DefId> = self.fn_params.keys().copied().collect();

        for def in defs {
            let Some(id) = types.def_types.get(&def).copied() else {
                continue;
            };

            let Type::Fn { params, ret } = types.interner.get(id) else {
                continue;
            };

            let names = self.fn_params.get(&def).cloned().unwrap_or_default();
            let mut args = Vec::with_capacity(params.len());

            for (index, param) in params.iter().enumerate() {
                if index == 0
                    && let Some(spelling) = self.method_self.get(&def)
                {
                    args.push(spelling.clone());
                    continue;
                }

                let name = names
                    .get(index)
                    .and_then(|name| name.as_deref())
                    .unwrap_or("_");
                let ty = types
                    .interner
                    .display_type(*param, interner.clone(), resolution);
                args.push(format!("{name}: {ty}"));
            }

            let ret = types
                .interner
                .display_type(*ret, interner.clone(), resolution);

            self.fn_param_labels.insert(def, args.clone());
            self.fn_rets.insert(def, ret.clone());
            self.fn_signatures
                .insert(def, format!("({}) {ret}", args.join(", ")));
        }
    }

    pub fn resolve_enum_payloads(
        &mut self,
        types: &TypeCheckResult,
        interner: Rc<RefCell<Rodeo>>,
        resolution: &ResolutionResult,
    ) {
        let borrowed = interner.borrow();

        for (enum_def, variants) in &types.enum_variants {
            let Some(info) = types.enum_info.get(enum_def) else {
                continue;
            };

            let Some(enum_name) = resolution
                .defs
                .get(enum_def)
                .map(|def| borrowed.resolve(&def.name).to_string())
            else {
                continue;
            };

            let mut payloads = Vec::with_capacity(variants.len());

            for variant in variants {
                let Some(entry) = info.variants.iter().find(|entry| entry.def_id == *variant)
                else {
                    continue;
                };

                let Some(name) = resolution
                    .defs
                    .get(variant)
                    .map(|def| borrowed.resolve(&def.name).to_string())
                else {
                    continue;
                };

                let payload = match &entry.payload {
                    Some(zeen_types::VariantPayload::Single(ty)) => Some(
                        types
                            .interner
                            .display_type(*ty, interner.clone(), resolution),
                    ),
                    _ => None,
                };

                payloads.push((name, payload));
            }

            self.enum_payloads.insert(enum_name, payloads);
        }
    }

    pub fn resolve_def_spans(
        &mut self,
        resolution: &ResolutionResult,
        filename: &str,
        canonical: &str,
    ) {
        let defs: HashSet<DefId> = self
            .method_calls
            .values()
            .copied()
            .chain(self.field_targets.values().map(|target| target.field_def))
            .collect();

        for def in defs {
            if self.def_spans.contains_key(&def) {
                continue;
            }

            record_def_span(&mut self.def_spans, resolution, def, filename, canonical);
        }
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

fn caps_display(types: &TypeCheckResult, id: TypeId) -> String {
    let copy = types.is_copy(id);

    let copy_word = if copy { "Copy" } else { "Move-only" };
    let drop_word = if needs_drop(types, id, copy) {
        "needs Drop"
    } else {
        "no Drop"
    };

    format!("{copy_word}, {drop_word}")
}

fn needs_drop(types: &TypeCheckResult, id: TypeId, copy: bool) -> bool {
    match types.interner.get(id) {
        Type::Struct { def_id, .. } => {
            types
                .struct_info
                .get(def_id)
                .is_some_and(|info| info.capabalities.has_explicit_drop)
                || !copy
        }
        Type::Enum { def_id, .. } => {
            types
                .enum_info
                .get(def_id)
                .is_some_and(|info| info.capabalities.has_explicit_drop)
                || !copy
        }
        Type::FatFn { .. } => true,
        _ => false,
    }
}

fn record_def_span(
    spans: &mut HashMap<DefId, (usize, usize, Option<String>)>,
    resolution: &ResolutionResult,
    def: DefId,
    filename: &str,
    canonical: &str,
) {
    let Some(info) = resolution.defs.get(&def) else {
        return;
    };

    let name = info.span.src.name();

    if name != filename && Some(name) != Some(canonical) {
        spans.insert(
            def,
            (
                info.span.span.offset(),
                info.span.span.len(),
                Some(name.to_string()),
            ),
        );
    } else {
        spans.insert(def, (info.span.span.offset(), info.span.span.len(), None));
    }
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn hir_fn_takes_self(func: &HirFn) -> bool {
    if func.self_param.is_some() {
        return true;
    }

    let Some(first) = func.params.first() else {
        return false;
    };

    let mut current = &first.ty.kind;

    loop {
        match current {
            HirTypeKind::SelfType(_) | HirTypeKind::SelfAlias(_) => return true,
            HirTypeKind::Const(inner) | HirTypeKind::SinglePointer(inner) => {
                current = &inner.kind;
            }
            _ => return false,
        }
    }
}

fn hir_fn_arity(func: &HirFn) -> usize {
    if func.self_param.is_some() {
        return func.params.len();
    }

    let Some(first) = func.params.first() else {
        return 0;
    };

    let mut current = &first.ty.kind;

    loop {
        match current {
            HirTypeKind::SelfType(_) | HirTypeKind::SelfAlias(_) => {
                return func.params.len().saturating_sub(1);
            }
            HirTypeKind::Const(inner) | HirTypeKind::SinglePointer(inner) => {
                current = &inner.kind;
            }
            _ => return func.params.len(),
        }
    }
}

struct Walker<'ctx> {
    resolution: &'ctx ResolutionResult,
    interner: std::cell::Ref<'ctx, Rodeo>,
    text: &'ctx str,
    filename: &'ctx str,
    canonical: &'ctx str,
    methods: HashSet<DefId>,
    analysis: Analysis,
    block_ends: Vec<usize>,
    local_scopes: HashMap<DefId, (usize, usize)>,
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

        let scope = def.and_then(|def| self.local_scopes.get(&def).copied());

        self.analysis.occurrences.push(Occurrence {
            offset,
            len,
            role,
            target_offset: target.0,
            target_len: target.1,
            target_file: target.2,
            target_module: None,
            hir,
            def,
            owner: None,
            type_def: None,
            ty: None,
            caps: None,
            is_pub: None,
            is_const: None,
            type_hint: false,
            scope,
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
            | DefKind::Builtin(_)
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
        let role = self.role_of_def(target)?;
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

    fn block_end(&self) -> usize {
        self.block_ends
            .last()
            .copied()
            .unwrap_or(self.text.len())
            .min(self.text.len())
    }

    fn record_method_scope(&mut self, owner: DefId, method: &HirDecl) {
        let start = method.source.span.offset();
        let end = start.saturating_add(method.source.span.len());

        self.analysis.scopes.push(Scope { start, end, owner });
    }

    fn record_impl_scope(&mut self, owner: DefId, decl: &HirDecl) {
        let start = decl.source.span.offset();
        let end = start.saturating_add(decl.source.span.len());

        self.analysis.scopes.push(Scope { start, end, owner });
    }

    fn method_member(&self, method: &HirDecl, is_interface: bool) -> Option<Member> {
        let HirDeclKind::Fn(func) = &method.kind else {
            return None;
        };

        Some(Member {
            name: self.interner.resolve(&func.name.0).to_string(),
            kind: MemberKind::Method,
            def: method.def_id,
            signature: None,
            is_static: !hir_fn_takes_self(func),
            is_pub: func.is_pub,
            is_interface,
            arity: Some(hir_fn_arity(func)),
        })
    }

    fn collect_interface(&mut self, name: String, methods: &[Rc<HirDecl>], source: &str) {
        for method in methods {
            let HirDeclKind::Fn(func) = &method.kind else {
                continue;
            };

            if let Some(member) = self.method_member(method, true) {
                self.analysis
                    .interfaces
                    .entry(name.clone())
                    .or_default()
                    .push(member);
            }

            self.record_fn_source_sig(source, func, method.def_id);
        }
    }

    fn collect_method(&mut self, owner: DefId, method: &HirDecl, is_interface: bool) {
        let Some(member) = self.method_member(method, is_interface) else {
            return;
        };

        self.analysis.member_owner.insert(method.def_id, owner);

        self.analysis.members.entry(owner).or_default().push(member);
    }

    fn walk_cross_file(&mut self, decl: &HirDecl) {
        if self.file_matches(decl.source.src.name()) {
            return;
        }

        let HirDeclKind::Interface(interface) = &decl.kind else {
            return;
        };

        let name = self.interner.resolve(&interface.name.0).to_string();
        let source = decl.source.src.inner().as_str();
        self.collect_interface(name, &interface.methods, source);

        for method in &interface.methods {
            self.collect_method(decl.def_id, method, true);
        }
    }

    fn walk_decl(&mut self, decl: &HirDecl) {
        if !self.file_matches(decl.source.src.name()) {
            return;
        }

        let start = decl.source.span.offset().min(self.text.len());
        let end = start
            .saturating_add(decl.source.span.len())
            .min(self.text.len());

        self.analysis.full_spans.insert(decl.def_id, (start, end));

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

                self.analysis
                    .type_kinds
                    .insert(decl.def_id, TypeKind::Struct);

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

                    self.analysis.member_owner.insert(field.def_id, decl.def_id);

                    self.analysis
                        .members
                        .entry(decl.def_id)
                        .or_default()
                        .push(Member {
                            name: self.interner.resolve(&field.name).to_string(),
                            kind: MemberKind::Field,
                            def: field.def_id,
                            signature: None,
                            is_static: false,
                            is_pub: field.is_pub,
                            is_interface: false,
                            arity: None,
                        });

                    self.walk_type(&field.ty);
                }

                for method in &strukt.methods {
                    self.collect_method(decl.def_id, method, false);
                    self.record_method_scope(decl.def_id, method);
                    self.walk_decl(method);
                }
            }

            HirDeclKind::Interface(interface) => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Type);

                self.analysis
                    .type_kinds
                    .insert(decl.def_id, TypeKind::Interface);

                self.emit_def(
                    interface.name.1.offset(),
                    interface.name.1.len(),
                    role,
                    Some(decl.def_id),
                );

                for generic in &interface.generics {
                    self.walk_generic(generic);
                }

                let name = self.interner.resolve(&interface.name.0).to_string();
                let text = self.text;
                self.collect_interface(name, &interface.methods, text);

                for method in &interface.methods {
                    self.collect_method(decl.def_id, method, true);
                    self.record_method_scope(decl.def_id, method);
                    self.walk_decl(method);
                }
            }

            HirDeclKind::Implement(implement) => {
                if let (Some(interface), Some(object)) = (implement.interface, implement.object) {
                    self.analysis.implements.push((interface, object));
                }

                for generic in &implement.generics {
                    self.walk_generic(generic);
                }

                for ty in &implement.object_generic_types {
                    self.walk_type(ty);
                }

                for method in &implement.methods {
                    if let Some(owner) = implement.object {
                        self.collect_method(owner, method, true);
                        self.record_method_scope(owner, method);
                        self.record_impl_scope(owner, decl);
                    }

                    self.walk_decl(method);
                }
            }

            HirDeclKind::Enum(enumeration) => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Type);

                self.analysis.type_kinds.insert(decl.def_id, TypeKind::Enum);

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
                        .member_owner
                        .insert(variant.def_id, decl.def_id);

                    self.analysis
                        .members
                        .entry(decl.def_id)
                        .or_default()
                        .push(Member {
                            name: self.interner.resolve(&variant.name).to_string(),
                            kind: MemberKind::Variant,
                            def: variant.def_id,
                            signature: None,
                            is_static: true,
                            is_pub: true,
                            is_interface: false,
                            arity: None,
                        });

                    match &variant.payload {
                        Some(HirEnumVariantPayload::Single(ty)) => self.walk_type(ty),
                        Some(HirEnumVariantPayload::Anonymous { fields, .. }) => {
                            for field in fields {
                                self.analysis
                                    .variant_fields
                                    .entry(variant.def_id)
                                    .or_default()
                                    .push(self.interner.resolve(&field.name).to_string());

                                self.walk_type(&field.ty);
                            }
                        }
                        None => {}
                    }
                }

                for method in &enumeration.methods {
                    self.collect_method(decl.def_id, method, false);
                    self.record_method_scope(decl.def_id, method);
                    self.walk_decl(method);
                }
            }

            HirDeclKind::Alias(alias) => {
                let role = self.role_of_def(decl.def_id).unwrap_or(Role::Type);

                self.analysis
                    .type_kinds
                    .insert(decl.def_id, TypeKind::Alias);

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

    fn record_fn_source_sig(&mut self, source: &str, func: &HirFn, def_id: DefId) {
        let name_start = func.name.1.offset().min(source.len());
        let name_end = name_start
            .saturating_add(func.name.1.len())
            .min(source.len());

        let Some(name) = source.get(name_start..name_end) else {
            return;
        };

        if name.is_empty() {
            return;
        }

        let limit = source.len().min(name_end.saturating_add(160));
        let mut paren = name_end;

        while paren < limit && source.as_bytes()[paren] != b'(' {
            paren += 1;
        }

        if paren >= limit {
            return;
        }

        let generics = source.get(name_end..paren).unwrap_or("").trim().to_string();
        let mut args = Vec::with_capacity(func.params.len());

        for param in &func.params {
            let start = param.span.offset().min(source.len());
            let end = start.saturating_add(param.span.len()).min(source.len());

            if let Some(spelling) = source.get(start..end).filter(|text| !text.is_empty()) {
                args.push(spelling.to_string());
            }
        }

        let mut code = format!("fn {name}{generics}({})", args.join(", "));

        if let Some(ret) = &func.return_type {
            let start = ret.source.span.offset().min(source.len());
            let end = start
                .saturating_add(ret.source.span.len())
                .min(source.len());

            if let Some(spelling) = source.get(start..end).filter(|text| !text.is_empty()) {
                code.push(' ');
                code.push_str(spelling);
            }
        }

        self.analysis.fn_source_sig.insert(def_id, code);
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

        let text = self.text;
        self.record_fn_source_sig(text, func, def_id);

        if !func.generics.is_empty() {
            self.analysis.fn_generics.insert(
                def_id,
                func.generics
                    .iter()
                    .map(|generic| self.interner.resolve(&generic.name.0).to_string())
                    .collect(),
            );
        }

        if hir_fn_takes_self(func)
            && let Some(first) = func.params.first()
        {
            let start = first.ty.source.span.offset().min(self.text.len());
            let end = start
                .saturating_add(first.ty.source.span.len())
                .min(self.text.len());

            if let Some(spelling) = self.text.get(start..end) {
                self.analysis
                    .method_self
                    .insert(def_id, spelling.to_string());
            }
        }

        for generic in &func.generics {
            self.walk_generic(generic);
        }

        if let Some(body) = &func.body {
            let start = body.source.span.offset().min(self.text.len());
            let end = start
                .saturating_add(body.source.span.len())
                .min(self.text.len());

            for param in &func.params {
                if let Some(def_id) = param.def_id {
                    self.local_scopes.insert(def_id, (start, end));
                }
            }
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

        if !generic.bounds.is_empty() {
            self.analysis
                .generic_bounds
                .insert(generic.def_id, generic.bounds.clone());
        }
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
                let start = stmt
                    .source
                    .span
                    .offset()
                    .saturating_add(stmt.source.span.len())
                    .min(self.text.len());
                let end = self.block_end();

                self.local_scopes.insert(*def_id, (start, end));

                if self
                    .emit_named(stmt.source.span, *name, role, Some(*def_id), None)
                    .is_some()
                    && explicit_type.is_none()
                    && let Some(last) = self.analysis.occurrences.last_mut()
                {
                    last.type_hint = true;
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
                let start = varname
                    .1
                    .offset()
                    .saturating_add(varname.1.len())
                    .min(self.text.len());
                let end = block
                    .source
                    .span
                    .offset()
                    .saturating_add(block.source.span.len())
                    .min(self.text.len());

                self.local_scopes.insert(*def_id, (start, end));

                if self
                    .emit_target(varname.1.offset(), varname.1.len(), role, *def_id, None)
                    .is_some()
                    && let Some(last) = self.analysis.occurrences.last_mut()
                {
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
            HirExprKind::VarRef {
                def: target,
                generic_args: _,
            }
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
                let field = match &callee.kind {
                    HirExprKind::FieldAccess { field, .. } => {
                        Some((field.1.offset(), field.1.len()))
                    }
                    _ => None,
                };

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
                    field,
                    explicit_generics: !generic_args.is_empty(),
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
                    let scope_end = arm
                        .body
                        .source
                        .span
                        .offset()
                        .saturating_add(arm.body.source.span.len())
                        .min(self.text.len());

                    self.walk_pattern(&arm.pattern, scope_end);

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

                self.analysis.raw_access.push(RawAccess {
                    access: expr.id,
                    field: (field.1.offset(), field.1.len()),
                });

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
                let end = expr
                    .source
                    .span
                    .offset()
                    .saturating_add(expr.source.span.len())
                    .min(self.text.len());

                self.block_ends.push(end);

                for stmt in stmts {
                    self.walk_stmt(stmt);
                }

                if let Some(trailing) = trailing {
                    self.walk_expr(trailing);
                }

                self.block_ends.pop();
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

    fn walk_pattern(&mut self, pattern: &HirPattern, scope_end: usize) {
        match pattern {
            HirPattern::Literal(_) | HirPattern::Wildcard | HirPattern::Range { .. } => {}

            HirPattern::Binding(binding) => self.walk_binding(binding, scope_end),

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
                    self.walk_binding(binding, scope_end);
                }
            }

            HirPattern::Or(patterns) => {
                for pattern in patterns {
                    self.walk_pattern(pattern, scope_end);
                }
            }
        }
    }

    fn walk_binding(&mut self, binding: &HirPatternBinding, scope_end: usize) {
        let role = self.role_of_def(binding.def_id).unwrap_or(Role::Variable);
        let start = binding
            .span
            .offset()
            .saturating_add(binding.span.len())
            .min(self.text.len());

        self.local_scopes.insert(binding.def_id, (start, scope_end));

        if self
            .emit_target(
                binding.span.offset(),
                binding.span.len(),
                role,
                binding.def_id,
                None,
            )
            .is_some()
            && let Some(last) = self.analysis.occurrences.last_mut()
        {
            last.type_hint = true;
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

pub fn build_syntax_fallback(program: &[&Declaration<'_>], text: &str) -> Analysis {
    let mut occurrences = Vec::new();

    for decl in program {
        walk_syntax_decl(decl, false, text, &mut occurrences);
    }

    occurrences.sort_by_key(|item| (item.offset, item.len));

    Analysis {
        occurrences,
        ..Default::default()
    }
}

pub fn merge_syntax_fallback(analysis: &mut Analysis, fallback: Analysis) {
    let existing: Vec<(usize, usize)> = analysis
        .occurrences
        .iter()
        .map(|occurrence| (occurrence.offset, occurrence.offset + occurrence.len))
        .collect();

    analysis
        .occurrences
        .extend(fallback.occurrences.into_iter().filter(|occurrence| {
            let start = occurrence.offset;
            let end = occurrence.offset + occurrence.len;

            !existing
                .iter()
                .any(|(other_start, other_end)| start < *other_end && *other_start < end)
        }));

    analysis
        .occurrences
        .sort_by_key(|occurrence| (occurrence.offset, occurrence.len));
}

fn span_end(span: SourceSpan) -> usize {
    span.offset().saturating_add(span.len())
}

fn push_syntax(
    occurrences: &mut Vec<Occurrence>,
    offset: usize,
    len: usize,
    role: Role,
    scope: Option<(usize, usize)>,
) {
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
        target_module: None,
        hir: None,
        def: None,
        owner: None,
        type_def: None,
        ty: None,
        caps: None,
        is_pub: None,
        is_const: None,
        type_hint: false,
        scope,
    });
}

fn syntax_param_name(text: &str, span: SourceSpan) -> Option<(usize, usize)> {
    let start = span.offset().min(text.len());
    let end = span_end(span).min(text.len());
    let slice = text.get(start..end)?;
    let bytes = slice.as_bytes();
    let mut cursor = 0;

    while cursor < bytes.len() && !is_ident_byte(bytes[cursor]) {
        cursor += 1;
    }

    let name_start = cursor;

    while cursor < bytes.len() && is_ident_byte(bytes[cursor]) {
        cursor += 1;
    }

    if cursor == name_start {
        return None;
    }

    let mut rest = cursor;

    while rest < bytes.len() && bytes[rest].is_ascii_whitespace() {
        rest += 1;
    }

    if bytes.get(rest) != Some(&b':') {
        return None;
    }

    Some((start + name_start, cursor - name_start))
}

fn syntax_let_name(text: &str, span: SourceSpan) -> Option<(usize, usize)> {
    let start = span.offset().min(text.len());
    let end = span_end(span).min(text.len());
    let slice = text.get(start..end)?;
    let bytes = slice.as_bytes();
    let mut cursor = 0;

    while cursor + 3 <= bytes.len() {
        let is_let = &bytes[cursor..cursor + 3] == b"let"
            && (cursor == 0 || !is_ident_byte(bytes[cursor - 1]))
            && (cursor + 3 == bytes.len() || !is_ident_byte(bytes[cursor + 3]));

        if is_let {
            cursor += 3;

            while cursor < bytes.len() && !is_ident_byte(bytes[cursor]) {
                cursor += 1;
            }

            let name_start = cursor;

            while cursor < bytes.len() && is_ident_byte(bytes[cursor]) {
                cursor += 1;
            }

            if cursor > name_start {
                return Some((start + name_start, cursor - name_start));
            }

            return None;
        }

        cursor += 1;
    }

    None
}

fn walk_syntax_decl(
    decl: &Declaration<'_>,
    method: bool,
    text: &str,
    occurrences: &mut Vec<Occurrence>,
) {
    match &decl.kind {
        DeclarationKind::FnDecl {
            name, params, body, ..
        } => {
            push_syntax(
                occurrences,
                name.1.offset(),
                name.1.len(),
                if method { Role::Method } else { Role::Function },
                None,
            );

            let Some(body) = body else {
                return;
            };

            let start = body.span.offset().min(text.len());
            let end = span_end(body.span).min(text.len());

            for param in *params {
                if let Some((offset, len)) = syntax_param_name(text, param.span) {
                    push_syntax(
                        occurrences,
                        offset,
                        len,
                        Role::Parameter,
                        Some((start, end)),
                    );
                }
            }

            walk_syntax_stmt(body, end, text, occurrences);
        }

        DeclarationKind::StructDecl { name, methods, .. }
        | DeclarationKind::ComptimeStruct { name, methods, .. }
        | DeclarationKind::InterfaceDecl { name, methods, .. } => {
            push_syntax(occurrences, name.1.offset(), name.1.len(), Role::Type, None);

            for method in *methods {
                walk_syntax_decl(method, true, text, occurrences);
            }
        }

        DeclarationKind::EnumDecl {
            name,
            variants,
            methods,
            ..
        } => {
            push_syntax(occurrences, name.1.offset(), name.1.len(), Role::Type, None);

            for variant in *variants {
                push_syntax(
                    occurrences,
                    variant.span.offset(),
                    variant.span.len(),
                    Role::EnumMember,
                    None,
                );
            }

            for method in *methods {
                walk_syntax_decl(method, true, text, occurrences);
            }
        }

        DeclarationKind::ImplementDecl { methods, .. } => {
            for method in *methods {
                walk_syntax_decl(method, true, text, occurrences);
            }
        }

        DeclarationKind::ExternVar { name, .. } | DeclarationKind::GlobalVar { name, .. } => {
            push_syntax(
                occurrences,
                name.1.offset(),
                name.1.len(),
                Role::Variable,
                None,
            );
        }

        DeclarationKind::Alias(alias) => {
            push_syntax(
                occurrences,
                alias.name.1.offset(),
                alias.name.1.len(),
                Role::Type,
                None,
            );
        }

        DeclarationKind::ComptimeConst { name, value, .. } => {
            let before = occurrences.len();

            push_syntax(
                occurrences,
                name.1.offset(),
                name.1.len(),
                Role::Variable,
                None,
            );

            if let Some(occurrence) = occurrences.get_mut(before) {
                occurrence.is_const = Some(true);
            }

            walk_syntax_expr(value, text, occurrences);
        }

        DeclarationKind::ComptimeFn {
            name, params, body, ..
        } => {
            push_syntax(
                occurrences,
                name.1.offset(),
                name.1.len(),
                Role::Function,
                None,
            );

            let Some(body) = body else {
                return;
            };

            let start = body.span.offset().min(text.len());
            let end = span_end(body.span).min(text.len());

            for param in *params {
                if let Some((offset, len)) = syntax_param_name(text, param.span) {
                    push_syntax(
                        occurrences,
                        offset,
                        len,
                        Role::Parameter,
                        Some((start, end)),
                    );
                }
            }

            walk_syntax_stmt(body, end, text, occurrences);
        }

        DeclarationKind::ComptimeBlock(block) => {
            for stmt in block.stmts.iter() {
                walk_syntax_stmt(stmt, text.len(), text, occurrences);
            }
        }

        DeclarationKind::Use { .. }
        | DeclarationKind::ExternLink { .. }
        | DeclarationKind::ExternInclude { .. }
        | DeclarationKind::ConditionalBlock(_) => {}
    }
}

fn walk_syntax_stmt(
    stmt: &Statement<'_>,
    block_end: usize,
    text: &str,
    occurrences: &mut Vec<Occurrence>,
) {
    match &stmt.kind {
        StatementKind::Let { value, .. } => {
            if let Some((offset, len)) = syntax_let_name(text, stmt.span) {
                let start = span_end(stmt.span).min(text.len());

                push_syntax(
                    occurrences,
                    offset,
                    len,
                    Role::Variable,
                    Some((start, block_end.min(text.len()))),
                );
            }

            if let Some(value) = value {
                walk_syntax_expr(value, text, occurrences);
            }
        }

        StatementKind::Assign { object, value }
        | StatementKind::CompoundAssign { object, value, .. } => {
            walk_syntax_expr(object, text, occurrences);
            walk_syntax_expr(value, text, occurrences);
        }

        StatementKind::Return { value } => {
            if let Some(value) = value {
                walk_syntax_expr(value, text, occurrences);
            }
        }

        StatementKind::Break | StatementKind::Continue => {}

        StatementKind::While { condition, block } => {
            walk_syntax_expr(condition, text, occurrences);
            walk_syntax_stmt(block, block_end, text, occurrences);
        }

        StatementKind::For {
            varname,
            iterator,
            block,
        } => {
            let start = span_end(varname.1).min(text.len());
            let end = span_end(block.span).min(text.len());

            push_syntax(
                occurrences,
                varname.1.offset(),
                varname.1.len(),
                Role::Variable,
                Some((start, end)),
            );

            walk_syntax_expr(iterator, text, occurrences);
            walk_syntax_stmt(block, end, text, occurrences);
        }

        StatementKind::Expr(expr) | StatementKind::TrailingExpr(expr) => {
            walk_syntax_expr(expr, text, occurrences);
        }

        StatementKind::FnDecl(decl) => walk_syntax_decl(decl, true, text, occurrences),

        StatementKind::ConditionalBlock(_) => {}
    }
}

fn walk_syntax_expr(expr: &Expression<'_>, text: &str, occurrences: &mut Vec<Occurrence>) {
    match &expr.kind {
        ExpressionKind::Block { stmts, trailing } => {
            let end = span_end(expr.span).min(text.len());

            for stmt in *stmts {
                walk_syntax_stmt(stmt, end, text, occurrences);
            }

            if let Some(trailing) = trailing {
                walk_syntax_expr(trailing, text, occurrences);
            }
        }

        ExpressionKind::If {
            condition,
            then_block,
            else_block,
        } => {
            walk_syntax_expr(condition, text, occurrences);
            walk_syntax_stmt(then_block, span_end(then_block.span), text, occurrences);

            if let Some(else_block) = else_block {
                walk_syntax_stmt(else_block, span_end(else_block.span), text, occurrences);
            }
        }

        ExpressionKind::Switch { object, arms } => {
            walk_syntax_expr(object, text, occurrences);

            for arm in *arms {
                let end = span_end(arm.body.span).min(text.len());

                walk_syntax_pattern(&arm.pattern, end, occurrences);

                if let Some(guard) = arm.guard {
                    walk_syntax_expr(guard, text, occurrences);
                }

                walk_syntax_expr(arm.body, text, occurrences);
            }
        }

        ExpressionKind::Closure { params, body, .. } => {
            let start = body.span.offset().min(text.len());
            let end = span_end(body.span).min(text.len());

            for param in *params {
                if let Some((offset, len)) = syntax_param_name(text, param.span) {
                    push_syntax(
                        occurrences,
                        offset,
                        len,
                        Role::Parameter,
                        Some((start, end)),
                    );
                }
            }

            walk_syntax_stmt(body, end, text, occurrences);
        }

        ExpressionKind::Call { callee, args } => {
            walk_syntax_expr(callee, text, occurrences);

            for arg in *args {
                walk_syntax_expr(arg, text, occurrences);
            }
        }

        ExpressionKind::MacroCall { args, .. } => {
            for arg in *args {
                walk_syntax_expr(arg, text, occurrences);
            }
        }

        ExpressionKind::Binary { lhs, rhs, .. } => {
            walk_syntax_expr(lhs, text, occurrences);
            walk_syntax_expr(rhs, text, occurrences);
        }

        ExpressionKind::Unary { expr, .. } => walk_syntax_expr(expr, text, occurrences),

        ExpressionKind::FieldAccess { object, .. } => {
            walk_syntax_expr(object, text, occurrences);
        }

        ExpressionKind::SliceAccess { object, index } => {
            walk_syntax_expr(object, text, occurrences);
            walk_syntax_expr(index, text, occurrences);
        }

        ExpressionKind::StructInit { ty, fields } => {
            walk_syntax_expr(ty, text, occurrences);

            if let Some(fields) = fields {
                for field in *fields {
                    walk_syntax_expr(field.value, text, occurrences);
                }
            }
        }

        ExpressionKind::ArrayInit { elements } => {
            for element in *elements {
                walk_syntax_expr(element, text, occurrences);
            }
        }

        ExpressionKind::ArrayRepeatInit { element, len } => {
            walk_syntax_expr(element, text, occurrences);
            walk_syntax_expr(len, text, occurrences);
        }

        ExpressionKind::Range { start, end, .. } => {
            if let Some(start) = start {
                walk_syntax_expr(start, text, occurrences);
            }

            if let Some(end) = end {
                walk_syntax_expr(end, text, occurrences);
            }
        }

        ExpressionKind::Literal(_)
        | ExpressionKind::Ident { .. }
        | ExpressionKind::Type(_)
        | ExpressionKind::TargetVar(_)
        | ExpressionKind::ConditionalBlock(_) => {}

        ExpressionKind::Decl(decl) => walk_syntax_decl(decl, false, text, occurrences),

        ExpressionKind::Splice { expr } => walk_syntax_expr(expr, text, occurrences),

        ExpressionKind::ComptimeBlock { stmts, trailing } => {
            let end = span_end(expr.span).min(text.len());

            for stmt in *stmts {
                walk_syntax_stmt(stmt, end, text, occurrences);
            }

            if let Some(trailing) = trailing {
                walk_syntax_expr(trailing, text, occurrences);
            }
        }
    }
}

fn walk_syntax_pattern(pattern: &Pattern<'_>, scope_end: usize, occurrences: &mut Vec<Occurrence>) {
    match pattern {
        Pattern::Named { span, .. } => {
            let start = span_end(*span);

            push_syntax(
                occurrences,
                span.offset(),
                span.len(),
                Role::Variable,
                Some((start, scope_end)),
            );
        }

        Pattern::EnumVariant {
            binding: Some((_, span)),
            ..
        } => {
            let start = span_end(*span);

            push_syntax(
                occurrences,
                span.offset(),
                span.len(),
                Role::Variable,
                Some((start, scope_end)),
            );
        }

        Pattern::Or(patterns) => {
            for pattern in *patterns {
                walk_syntax_pattern(pattern, scope_end, occurrences);
            }
        }

        Pattern::Literal(_)
        | Pattern::EnumVariant { .. }
        | Pattern::Range { .. }
        | Pattern::Wildcard => {}
    }
}

pub fn build_text_fallback(text: &str, offset: usize) -> Analysis {
    let offset = offset.min(text.len());
    let Some((header_start, body_start, body_end)) = enclosing_fn_body(text, offset) else {
        return Analysis::default();
    };

    let pairs = brace_pairs(text);
    let mut occurrences = Vec::new();

    if let Some(open) = text
        .get(header_start..body_start)
        .and_then(|header| header.rfind('('))
        .map(|position| header_start + position)
        && let Some(close) = paren_close(text, open)
    {
        let mut depth = 0i32;
        let mut segment = open + 1;

        for cursor in open + 1..=close {
            match text.as_bytes().get(cursor) {
                Some(b'[') | Some(b'(') | Some(b'{') => depth += 1,
                Some(b']') | Some(b')') | Some(b'}') => depth -= 1,
                Some(b',') if depth == 0 => {
                    push_text_param(
                        text,
                        segment,
                        cursor,
                        body_start,
                        body_end,
                        &mut occurrences,
                    );
                    segment = cursor + 1;
                }
                _ => {}
            }
        }

        push_text_param(text, segment, close, body_start, body_end, &mut occurrences);
    }

    let mut stack: Vec<usize> = vec![body_start];
    let mut cursor = body_start;
    let limit = offset.min(body_end);

    while cursor < limit {
        match text.as_bytes()[cursor] {
            b'"' => cursor = text_skip_string(text, cursor),
            b'/' if text.as_bytes().get(cursor + 1) == Some(&b'/') => {
                cursor = text_skip_line(text, cursor)
            }
            b'/' if text.as_bytes().get(cursor + 1) == Some(&b'*') => {
                cursor = text_skip_comment(text, cursor)
            }
            b'{' => {
                stack.push(cursor);
                cursor += 1;
            }
            b'}' => {
                if stack.len() > 1 {
                    stack.pop();
                }

                cursor += 1;
            }
            b'l' if text
                .get(cursor..)
                .is_some_and(|rest| rest.starts_with("let"))
                && (cursor == 0 || !is_ident_byte(text.as_bytes()[cursor - 1]))
                && text
                    .as_bytes()
                    .get(cursor + 3)
                    .is_some_and(|byte| !is_ident_byte(*byte)) =>
            {
                let mut name_start = cursor + 3;

                while name_start < text.len() && !is_ident_byte(text.as_bytes()[name_start]) {
                    name_start += 1;
                }

                let mut name_end = name_start;

                while name_end < text.len() && is_ident_byte(text.as_bytes()[name_end]) {
                    name_end += 1;
                }

                if name_end > name_start {
                    let block_end = stack
                        .last()
                        .and_then(|open| pairs.get(open))
                        .copied()
                        .unwrap_or(body_end)
                        .min(body_end);
                    let mut statement_end = name_end;

                    while statement_end < block_end
                        && !matches!(text.as_bytes()[statement_end], b';' | b'\n')
                    {
                        statement_end += 1;
                    }

                    let start = if text.as_bytes().get(statement_end) == Some(&b';') {
                        statement_end + 1
                    } else {
                        statement_end
                    };

                    push_syntax(
                        &mut occurrences,
                        name_start,
                        name_end - name_start,
                        Role::Variable,
                        Some((start, block_end)),
                    );
                }

                cursor = name_end.max(cursor + 3);
            }
            _ => cursor += 1,
        }
    }

    occurrences.sort_by_key(|item| (item.offset, item.len));

    Analysis {
        occurrences,
        ..Default::default()
    }
}

fn push_text_param(
    text: &str,
    start: usize,
    end: usize,
    scope_start: usize,
    scope_end: usize,
    occurrences: &mut Vec<Occurrence>,
) {
    let end = end.min(text.len());
    let Some(slice) = text.get(start.min(end)..end) else {
        return;
    };
    let bytes = slice.as_bytes();
    let mut colon = None;
    let mut depth = 0i32;

    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'[' | b'(' | b'{' => depth += 1,
            b']' | b')' | b'}' => depth -= 1,
            b':' if depth == 0 => {
                colon = Some(index);
                break;
            }
            _ => {}
        }
    }

    let Some(colon) = colon else {
        return;
    };

    let mut cursor = colon;

    while cursor > 0 && text.as_bytes()[start + cursor - 1].is_ascii_whitespace() {
        cursor -= 1;
    }

    let name_end = cursor;

    while cursor > 0 && is_ident_byte(text.as_bytes()[start + cursor - 1]) {
        cursor -= 1;
    }

    if name_end > cursor {
        push_syntax(
            occurrences,
            start + cursor,
            name_end - cursor,
            Role::Parameter,
            Some((scope_start, scope_end)),
        );
    }
}

fn enclosing_fn_body(text: &str, offset: usize) -> Option<(usize, usize, usize)> {
    let pairs = brace_pairs(text);
    let mut candidates: Vec<(usize, usize)> = pairs
        .iter()
        .filter(|(open, close)| **open < offset && offset <= **close)
        .map(|(open, close)| (*open, *close))
        .collect();

    candidates.sort_by_key(|(open, _)| std::cmp::Reverse(*open));

    for (open, close) in candidates {
        let header_start = text
            .get(..open)
            .and_then(|head| head.rfind('}'))
            .map(|position| position + 1)
            .unwrap_or(0);

        if let Some(header) = text.get(header_start..open)
            && header.contains("fn ")
        {
            return Some((header_start, open, close));
        }
    }

    None
}

fn paren_close(text: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut cursor = open;

    while cursor < text.len() {
        match text.as_bytes()[cursor] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;

                if depth == 0 {
                    return Some(cursor);
                }
            }
            _ => {}
        }

        cursor += 1;
    }

    None
}

fn brace_pairs(text: &str) -> HashMap<usize, usize> {
    let bytes = text.as_bytes();
    let mut stack = Vec::new();
    let mut pairs = HashMap::new();
    let mut cursor = 0;

    while cursor < bytes.len() {
        match bytes[cursor] {
            b'"' => cursor = text_skip_string(text, cursor),
            b'/' if bytes.get(cursor + 1) == Some(&b'/') => cursor = text_skip_line(text, cursor),
            b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                cursor = text_skip_comment(text, cursor)
            }
            b'{' => {
                stack.push(cursor);
                cursor += 1;
            }
            b'}' => {
                if let Some(open) = stack.pop() {
                    pairs.insert(open, cursor);
                }

                cursor += 1;
            }
            _ => cursor += 1,
        }
    }

    pairs
}

fn text_skip_string(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let mut cursor = start + 1;

    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor += 2,
            b'"' => return cursor + 1,
            _ => cursor += 1,
        }
    }

    bytes.len()
}

fn text_skip_line(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let mut cursor = start;

    while cursor < bytes.len() && bytes[cursor] != b'\n' {
        cursor += 1;
    }

    cursor
}

fn text_skip_comment(text: &str, start: usize) -> usize {
    let bytes = text.as_bytes();
    let mut cursor = start + 2;

    while cursor + 1 < bytes.len() {
        if bytes[cursor] == b'*' && bytes[cursor + 1] == b'/' {
            return cursor + 2;
        }

        cursor += 1;
    }

    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_lsp_server::ls_types::Uri;

    const GENERIC_CALL: &str = "fn max[T](a: T, b: T) T {\n    return a;\n}\n\nfn main() {\n    let m = max(1, 2);\n    @println(\"{}\", m);\n}\n";

    #[test]
    fn records_inferred_generics_at_call_sites() {
        let uri: Uri = "file:///tmp/zeen-lsp-analysis-generics.zn".parse().unwrap();
        let output = crate::diagnostics::check(&uri, GENERIC_CALL, &[]);
        assert!(output.diagnostics.is_empty());

        let call_at = GENERIC_CALL.rfind("max(").unwrap();
        let key = (call_at, "max".len());
        assert_eq!(
            output.analysis.call_generics.get(&key).map(String::as_str),
            Some("[i32]")
        );
    }

    const COMPTIME_SOURCE: &str = "comptime LIMIT = 3;\n\ncomptime fn doubled(n: comptime_int) comptime_int {\n    return n * 2;\n}\n\nfn main() {\n    let value = comptime {\n        let local = doubled(LIMIT);\n        local\n    };\n\n    @println(\"{}\", value);\n}\n";

    #[test]
    fn records_comptime_names() {
        let uri: Uri = "file:///tmp/zeen-lsp-analysis-comptime.zn".parse().unwrap();
        let output = crate::diagnostics::check(&uri, COMPTIME_SOURCE, &[]);
        assert!(output.diagnostics.is_empty());

        let occurrences = &output.analysis.occurrences;
        let matches = |name: &str, role: Role| {
            occurrences.iter().any(|occurrence| {
                COMPTIME_SOURCE.get(occurrence.offset..occurrence.offset + occurrence.len)
                    == Some(name)
                    && occurrence.role == role
            })
        };

        assert!(matches("LIMIT", Role::Variable));
        assert!(matches("doubled", Role::Function));
        assert!(matches("local", Role::Variable));
        assert!(matches("n", Role::Parameter));
    }

    const IMPLEMENT_SOURCE: &str = "struct Point {\n    pub x: i32,\n    pub y: i32,\n}\n\nimplement Point {\n    pub fn kind(*const self) i32 {\n        return self.x + self.y;\n    }\n\n    pub fn origin() Point {\n        return Point { .x = 0, .y = 0 };\n    }\n}\n\nfn main() {\n    let p = Point.origin();\n    @println(\"{}\", p.kind());\n}\n";

    #[test]
    fn inherent_implement_methods_resolve_uses() {
        let uri: Uri = "file:///tmp/zeen-lsp-analysis-implement.zn"
            .parse()
            .unwrap();
        let output = crate::diagnostics::check(&uri, IMPLEMENT_SOURCE, &[]);
        assert!(output.diagnostics.is_empty());

        let named: Vec<&Occurrence> = output
            .analysis
            .occurrences
            .iter()
            .filter(|occurrence| {
                IMPLEMENT_SOURCE.get(occurrence.offset..occurrence.offset + occurrence.len)
                    == Some("kind")
            })
            .collect();

        assert_eq!(named.len(), 2);

        let declaration = named
            .iter()
            .find(|occurrence| occurrence.def.is_some())
            .unwrap();
        let call = named
            .iter()
            .find(|occurrence| occurrence.offset != declaration.offset)
            .unwrap();

        assert_eq!(declaration.role, Role::Method);
        assert_eq!(
            crate::hover::method_at(&output.analysis, (call.offset, call.len)),
            declaration.def
        );
    }

    const UNNAMED_PARAM: &str = "fn put(i32) {\n}\n\nfn main() {\n    put(5);\n}\n";

    #[test]
    fn records_parameter_types_for_unnamed_params() {
        let uri: Uri = "file:///tmp/zeen-lsp-analysis-unnamed.zn".parse().unwrap();
        let output = crate::diagnostics::check(&uri, UNNAMED_PARAM, &[]);
        assert!(output.diagnostics.is_empty());

        let call = output
            .analysis
            .calls
            .iter()
            .find(|call| call.callee == "put")
            .unwrap();

        assert_eq!(call.params, vec![None]);
        assert_eq!(call.param_types, vec![Some("i32".to_string())]);
    }
}
