use std::collections::HashSet;

use tower_lsp_server::ls_types::{CompletionItem, CompletionItemKind, InsertTextFormat};
use zeen_hir::HirMacroKind;
use zeen_lexer::token::{CompilerKeyword, CompilerType};

use crate::analysis::{Analysis, Member, MemberKind, Role};
use crate::modules::{ModuleKind, ModuleMember};

#[derive(Debug, Clone, Copy, Default)]
pub struct ResolvePaths<'a> {
    pub dir: Option<&'a std::path::Path>,
    pub root: Option<&'a std::path::Path>,
    pub std: Option<&'a std::path::Path>,
    pub packages: Option<&'a [(String, std::path::PathBuf)]>,
}

impl<'a> ResolvePaths<'a> {
    pub fn project(&self) -> Option<&'a std::path::Path> {
        self.root.or(self.dir)
    }

    pub fn packages(&self) -> &'a [(String, std::path::PathBuf)] {
        self.packages.unwrap_or(&[])
    }
}

const SNIPPETS: &[(&str, &str)] = &[
    ("fn", "fn ${1:name}(${2:params}) ${3:Ret} {\n\t$0\n}"),
    ("struct", "struct ${1:Name} {\n\t$0\n}"),
    ("if", "if (${1:cond}) {\n\t$0\n}"),
    ("while", "while (${1:cond}) {\n\t$0\n}"),
    ("for", "for (${1:item} : ${2:iter}) {\n\t$0\n}"),
    ("let", "let ${1:name} = $0;"),
];

fn role_kind(role: Role) -> CompletionItemKind {
    match role {
        Role::Function => CompletionItemKind::FUNCTION,
        Role::Method => CompletionItemKind::METHOD,
        Role::Type => CompletionItemKind::STRUCT,
        Role::Variable => CompletionItemKind::VARIABLE,
        Role::Parameter => CompletionItemKind::VARIABLE,
        Role::Property => CompletionItemKind::FIELD,
        Role::EnumMember => CompletionItemKind::ENUM_MEMBER,
    }
}

fn ident_prefix(text: &str, offset: usize) -> &str {
    let offset = offset.min(text.len());
    let bytes = text.as_bytes();
    let mut start = offset;

    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }

    &text[start..offset]
}

pub fn complete(
    text: &str,
    analysis: &Analysis,
    offset: usize,
    paths: ResolvePaths<'_>,
) -> Vec<CompletionItem> {
    let prefix = ident_prefix(text, offset);
    let mut seen: HashSet<String> = HashSet::new();
    let mut items = Vec::new();

    for (trigger, body) in SNIPPETS {
        if trigger.starts_with(prefix) {
            items.push(CompletionItem {
                label: trigger.to_string(),
                kind: Some(CompletionItemKind::SNIPPET),
                insert_text: Some(body.to_string()),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            });
        }
    }

    for keyword in CompilerKeyword::all_names() {
        if keyword.starts_with(prefix) && seen.insert(keyword.to_string()) {
            items.push(CompletionItem {
                label: keyword.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }
    }

    for builtin in CompilerType::all_names() {
        if builtin.starts_with(prefix) && seen.insert(builtin.clone()) {
            items.push(CompletionItem {
                label: builtin,
                kind: Some(CompletionItemKind::STRUCT),
                ..Default::default()
            });
        }
    }

    for name in HirMacroKind::all_names() {
        let label = format!("@{name}");
        let trimmed = label.trim_start_matches('@');

        if trimmed.starts_with(prefix) && seen.insert(label.clone()) {
            items.push(CompletionItem {
                label,
                kind: Some(CompletionItemKind::FUNCTION),
                ..Default::default()
            });
        }
    }

    for occurrence in &analysis.occurrences {
        let Some(name) = text.get(occurrence.offset..occurrence.offset + occurrence.len) else {
            continue;
        };

        if name.is_empty() || !name.starts_with(prefix) || !seen.insert(name.to_string()) {
            continue;
        }

        if matches!(
            occurrence.role,
            Role::Property | Role::Method | Role::Function
        ) && occurrence.is_pub == Some(false)
            && let Some(owner) = occurrence.owner
            && !scope_covers(analysis, owner, offset)
        {
            continue;
        }

        items.push(CompletionItem {
            label: name.to_string(),
            kind: Some(role_kind(occurrence.role)),
            detail: occurrence.ty.clone(),
            ..Default::default()
        });
    }

    if let (Some(dir), Some(root)) = (paths.dir, paths.project()) {
        for import in crate::modules::use_imports(text) {
            for member in
                crate::modules::members_of_use(dir, root, paths.std, paths.packages(), &import)
            {
                if !member.name.starts_with(prefix) || !seen.insert(member.name.clone()) {
                    continue;
                }

                items.push(CompletionItem {
                    label: member.name.clone(),
                    kind: Some(module_kind(&member.kind)),
                    detail: Some(member.module.clone()),
                    ..Default::default()
                });
            }
        }
    }

    items
}

fn module_kind(kind: &ModuleKind) -> CompletionItemKind {
    match kind {
        ModuleKind::Function => CompletionItemKind::FUNCTION,
        ModuleKind::Struct => CompletionItemKind::STRUCT,
        ModuleKind::Enum => CompletionItemKind::ENUM,
        ModuleKind::Interface => CompletionItemKind::INTERFACE,
        ModuleKind::Const => CompletionItemKind::CONSTANT,
        ModuleKind::Alias => CompletionItemKind::STRUCT,
        ModuleKind::Method => CompletionItemKind::METHOD,
        ModuleKind::Field => CompletionItemKind::FIELD,
        ModuleKind::Variant => CompletionItemKind::ENUM_MEMBER,
    }
}

pub(crate) fn normalize_type_name(ty: &str) -> Option<String> {
    let mut name = ty.trim();

    loop {
        if let Some(rest) = name.strip_prefix("[*]") {
            name = rest.trim_start();
            continue;
        }

        if let Some(rest) = name.strip_prefix("[]") {
            name = rest.trim_start();
            continue;
        }

        if let Some(rest) = name.strip_prefix('*') {
            name = rest.trim_start();
            continue;
        }

        if let Some(rest) = name.strip_prefix("const ") {
            name = rest.trim_start();
            continue;
        }

        if let Some(rest) = name.strip_prefix("const\t") {
            name = rest.trim_start();
            continue;
        }

        break;
    }

    if name.starts_with('[') {
        return None;
    }

    let mut end = name.len();

    for (index, byte) in name.bytes().enumerate() {
        if byte == b'[' || byte == b'(' || byte == b' ' || byte == b'\t' {
            end = index;
            break;
        }
    }

    name = &name[..end];

    if name.is_empty()
        || name == "fn"
        || name == "Fn"
        || name == "FnOnce"
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return None;
    }

    Some(name.to_string())
}

fn forward_ident(text: &str, offset: usize) -> &str {
    let offset = offset.min(text.len());
    let bytes = text.as_bytes();
    let mut end = offset;

    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }

    &text[offset..end]
}

fn find_type_def(analysis: &Analysis, text: &str, name: &str) -> Option<zeen_resolve::DefId> {
    analysis.occurrences.iter().find_map(|occurrence| {
        if occurrence.role == Role::Type
            && text.get(occurrence.offset..occurrence.offset + occurrence.len) == Some(name)
        {
            occurrence.def
        } else {
            None
        }
    })
}

fn find_variant_def(analysis: &Analysis, text: &str, name: &str) -> Option<zeen_resolve::DefId> {
    analysis.occurrences.iter().find_map(|occurrence| {
        if occurrence.role == Role::EnumMember
            && text.get(occurrence.offset..occurrence.offset + occurrence.len) == Some(name)
        {
            occurrence.def
        } else {
            None
        }
    })
}

pub fn scope_covers(analysis: &Analysis, owner: zeen_resolve::DefId, cursor: usize) -> bool {
    analysis
        .scopes
        .iter()
        .any(|scope| scope.owner == owner && scope.start <= cursor && cursor <= scope.end)
}

fn member_completion_kind(member: &Member) -> CompletionItemKind {
    match member.kind {
        MemberKind::Method if member.is_static => CompletionItemKind::FUNCTION,
        MemberKind::Method => CompletionItemKind::METHOD,
        MemberKind::Field => CompletionItemKind::FIELD,
        MemberKind::Variant => CompletionItemKind::ENUM_MEMBER,
    }
}

fn module_member_completion_kind(member: &ModuleMember) -> CompletionItemKind {
    match &member.kind {
        ModuleKind::Method if member.is_static => CompletionItemKind::FUNCTION,
        ModuleKind::Method => CompletionItemKind::METHOD,
        ModuleKind::Field => CompletionItemKind::FIELD,
        ModuleKind::Variant => CompletionItemKind::ENUM_MEMBER,
        _ => CompletionItemKind::FIELD,
    }
}

fn keep_local_member(member: &Member, static_context: Option<bool>) -> bool {
    match (static_context, &member.kind) {
        (Some(true), MemberKind::Method) => member.is_static,
        (Some(true), MemberKind::Variant) => true,
        (Some(true), MemberKind::Field) => false,
        (Some(false), MemberKind::Method) => !member.is_static,
        (Some(false), MemberKind::Field) => true,
        (Some(false), MemberKind::Variant) => false,
        (None, _) => true,
    }
}

fn keep_module_member(member: &ModuleMember, static_context: Option<bool>) -> bool {
    match (static_context, &member.kind) {
        (Some(true), ModuleKind::Method) => member.is_static,
        (Some(true), ModuleKind::Variant) => true,
        (Some(true), _) => false,
        (Some(false), ModuleKind::Method) => !member.is_static,
        (Some(false), ModuleKind::Field) => true,
        (Some(false), _) => false,
        (None, ModuleKind::Method | ModuleKind::Field | ModuleKind::Variant) => true,
        (None, _) => false,
    }
}

fn member_rank(is_static_item: bool, static_context: Option<bool>) -> u8 {
    match static_context {
        Some(_) => 0,
        None => {
            if is_static_item {
                0
            } else {
                1
            }
        }
    }
}

pub fn dot_complete(
    text: &str,
    analysis: &Analysis,
    dot_offset: usize,
    paths: ResolvePaths<'_>,
) -> Vec<CompletionItem> {
    let bytes = text.as_bytes();
    let mut start = dot_offset;

    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }

    if start == dot_offset {
        return Vec::new();
    }

    let Some(receiver_name) = text.get(start..dot_offset) else {
        return Vec::new();
    };

    let filter = forward_ident(text, dot_offset + 1).to_string();
    let mut static_context: Option<bool> = None;
    let mut type_name: Option<String> = None;

    if let Some(ty) = analysis
        .access_types
        .get(&(start, dot_offset - start))
        .and_then(|ty| normalize_type_name(ty))
    {
        static_context = Some(false);
        type_name = Some(ty);
    } else if let Some(receiver) = analysis.at(start) {
        if receiver.role == Role::Type {
            if let Some(name) = text.get(receiver.offset..receiver.offset + receiver.len) {
                static_context = Some(true);
                type_name = Some(name.to_string());
            }
        } else if let Some(ty) = receiver.ty.as_deref().and_then(normalize_type_name) {
            static_context = Some(false);
            type_name = Some(ty);
        }
    }

    let lookup = type_name.as_deref().unwrap_or(receiver_name).to_string();

    let mut items = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    push_type_members(
        analysis,
        text,
        &lookup,
        static_context,
        dot_offset,
        &filter,
        paths,
        &mut items,
        &mut seen,
    );

    items
}

#[allow(clippy::too_many_arguments)]
fn push_type_members(
    analysis: &Analysis,
    text: &str,
    lookup: &str,
    static_context: Option<bool>,
    cursor: usize,
    filter: &str,
    paths: ResolvePaths<'_>,
    items: &mut Vec<CompletionItem>,
    seen: &mut std::collections::HashSet<String>,
) {
    if let Some(def) = find_type_def(analysis, text, lookup)
        && let Some(members) = analysis.members.get(&def)
    {
        let allow_private = scope_covers(analysis, def, cursor);
        let mut local: Vec<&Member> = members
            .iter()
            .filter(|member| {
                (member.is_pub || allow_private)
                    && keep_local_member(member, static_context)
                    && member.name.starts_with(filter)
            })
            .collect();
        local.sort_by(|a, b| a.name.cmp(&b.name));

        for member in local {
            if seen.insert(member.name.clone()) {
                let rank = member_rank(
                    member.kind == MemberKind::Variant
                        || (member.kind == MemberKind::Method && member.is_static),
                    static_context,
                );
                items.push(CompletionItem {
                    label: member.name.clone(),
                    kind: Some(member_completion_kind(member)),
                    detail: member.signature.clone(),
                    sort_text: Some(format!("{rank}_{}", member.name)),
                    ..Default::default()
                });
            }
        }
    }

    if let (Some(dir), Some(root)) = (paths.dir, paths.project()) {
        let mut imported = crate::modules::type_members(
            dir,
            root,
            paths.std,
            paths.packages(),
            &crate::modules::use_imports(text),
            lookup,
        );
        imported.sort_by(|a, b| a.name.cmp(&b.name));

        for member in imported {
            if !member.name.starts_with(filter) || !keep_module_member(&member, static_context) {
                continue;
            }

            if seen.insert(member.name.clone()) {
                let rank = member_rank(
                    member.kind == ModuleKind::Variant
                        || (member.kind == ModuleKind::Method && member.is_static),
                    static_context,
                );
                items.push(CompletionItem {
                    label: member.name.clone(),
                    kind: Some(module_member_completion_kind(&member)),
                    detail: Some(member.module.clone()),
                    sort_text: Some(format!("{rank}_{}", member.name)),
                    ..Default::default()
                });
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallTrigger {
    pub callee: (usize, usize),
    pub dot: usize,
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn match_open_paren(text: &str, close: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth: usize = 0;
    let mut index = close;

    loop {
        match bytes.get(index) {
            Some(b')') => depth += 1,
            Some(b'(') => {
                depth = depth.saturating_sub(1);

                if depth == 0 {
                    return Some(index);
                }
            }
            Some(_) => {}
            None => return None,
        }

        if index == 0 {
            return None;
        }

        index -= 1;
    }
}

fn expr_start_before(text: &str, open_paren: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut pos = open_paren;
    let mut start = None;

    loop {
        while pos > 0 && bytes[pos - 1].is_ascii_whitespace() {
            pos -= 1;
        }

        let mut head = pos;

        while head > 0 && is_ident_byte(bytes[head - 1]) {
            head -= 1;
        }

        if head == pos {
            return start;
        }

        start = Some(head);

        let mut tail = head;

        while tail > 0 && bytes[tail - 1].is_ascii_whitespace() {
            tail -= 1;
        }

        if tail == 0 || bytes[tail - 1] != b'.' {
            return start;
        }

        let mut before = tail - 1;

        while before > 0 && bytes[before - 1].is_ascii_whitespace() {
            before -= 1;
        }

        if before > 0 && bytes[before - 1] == b')' {
            pos = match_open_paren(text, before - 1)?;
            continue;
        }

        pos = before;
    }
}

pub fn call_dot_before(text: &str, offset: usize) -> Option<CallTrigger> {
    let offset = offset.min(text.len());
    let bytes = text.as_bytes();
    let mut end = offset;

    while end > 0 && is_ident_byte(bytes[end - 1]) {
        end -= 1;
    }

    if end == 0 || bytes[end - 1] != b'.' {
        return None;
    }

    let dot = end - 1;

    if dot == 0 || bytes[dot - 1] != b')' {
        return None;
    }

    let open = match_open_paren(text, dot - 1)?;
    let start = expr_start_before(text, open)?;

    Some(CallTrigger {
        callee: (start, open - start),
        dot,
    })
}

pub fn call_complete(
    text: &str,
    analysis: &Analysis,
    trigger: CallTrigger,
    paths: ResolvePaths<'_>,
) -> Vec<CompletionItem> {
    let Some(ty) = analysis
        .call_types
        .get(&trigger.callee)
        .and_then(|ty| normalize_type_name(ty))
    else {
        return Vec::new();
    };

    let filter = forward_ident(text, trigger.dot + 1).to_string();
    let mut items = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    push_type_members(
        analysis,
        text,
        &ty,
        Some(false),
        trigger.dot,
        &filter,
        paths,
        &mut items,
        &mut seen,
    );

    items
}

pub fn struct_init_context(text: &str, offset: usize) -> Option<(String, usize)> {
    let offset = offset.min(text.len());
    let bytes = text.as_bytes();
    let mut depth: usize = 0;
    let mut index = offset;

    let open = loop {
        if index == 0 {
            return None;
        }

        index -= 1;

        match bytes[index] {
            b'}' => depth += 1,
            b'{' => {
                if depth == 0 {
                    break index;
                }

                depth -= 1;
            }
            _ => {}
        }
    };

    let mut cursor = open;

    while cursor > 0 && bytes[cursor - 1].is_ascii_whitespace() {
        cursor -= 1;
    }

    if cursor > 0 && bytes[cursor - 1] == b']' {
        let mut brackets: usize = 0;
        let mut inner = cursor;

        loop {
            if inner == 0 {
                return None;
            }

            inner -= 1;

            match bytes[inner] {
                b']' => brackets += 1,
                b'[' => {
                    if brackets == 0 {
                        return None;
                    }

                    brackets -= 1;

                    if brackets == 0 {
                        cursor = inner;
                        break;
                    }
                }
                _ => {}
            }
        }

        while cursor > 0 && bytes[cursor - 1].is_ascii_whitespace() {
            cursor -= 1;
        }
    }

    let mut start = cursor;

    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }

    if start == cursor {
        return None;
    }

    text.get(start..cursor)
        .filter(|name| !name.is_empty())
        .map(|name| (name.to_string(), open))
}

fn struct_init_used_fields(text: &str, open: usize) -> std::collections::HashSet<String> {
    let bytes = text.as_bytes();
    let mut depth: usize = 0;
    let mut end = text.len();

    for (position, byte) in bytes.iter().enumerate().skip(open + 1) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                if depth == 0 {
                    end = position;
                    break;
                }

                depth -= 1;
            }
            _ => {}
        }
    }

    let Some(body) = text.get(open + 1..end.min(text.len())) else {
        return std::collections::HashSet::new();
    };

    let mut used = std::collections::HashSet::new();

    for (position, current) in body.char_indices() {
        if current != '.' {
            continue;
        }

        let name_start = position + 1;
        let mut name_end = name_start;

        while name_end < body.len() && {
            let byte = body.as_bytes()[name_end];
            byte.is_ascii_alphanumeric() || byte == b'_'
        } {
            name_end += 1;
        }

        if name_end == name_start {
            continue;
        }

        let mut rest = name_end;

        while rest < body.len() && body.as_bytes()[rest].is_ascii_whitespace() {
            rest += 1;
        }

        if rest < body.len()
            && body.as_bytes()[rest] == b'='
            && let Some(field) = body.get(name_start..name_end)
        {
            used.insert(field.to_string());
        }
    }

    used
}

pub fn struct_init_complete(
    text: &str,
    analysis: &Analysis,
    offset: usize,
    paths: ResolvePaths<'_>,
) -> Vec<CompletionItem> {
    let Some((type_name, open)) = struct_init_context(text, offset) else {
        return Vec::new();
    };

    let filter = ident_prefix(text, offset).to_string();
    let used = struct_init_used_fields(text, open);
    let mut items = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    if let Some(def) = find_type_def(analysis, text, &type_name)
        && let Some(members) = analysis.members.get(&def)
    {
        let allow_private = scope_covers(analysis, def, offset);
        let mut local: Vec<&Member> = members
            .iter()
            .filter(|member| {
                member.kind == MemberKind::Field
                    && (member.is_pub || allow_private)
                    && !used.contains(&member.name)
                    && member.name.starts_with(&filter)
            })
            .collect();
        local.sort_by(|a, b| a.name.cmp(&b.name));

        for member in local {
            if seen.insert(member.name.clone()) {
                items.push(CompletionItem {
                    label: member.name.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: member.signature.clone(),
                    sort_text: Some(format!("0_{}", member.name)),
                    ..Default::default()
                });
            }
        }
    }

    if items.is_empty()
        && let Some(def) = find_variant_def(analysis, text, &type_name)
        && let Some(fields) = analysis.variant_fields.get(&def)
    {
        let mut names: Vec<&str> = fields
            .iter()
            .map(String::as_str)
            .filter(|name| !used.contains(*name) && name.starts_with(&filter))
            .collect();
        names.sort();

        for name in names {
            if seen.insert(name.to_string()) {
                items.push(CompletionItem {
                    label: name.to_string(),
                    kind: Some(CompletionItemKind::FIELD),
                    sort_text: Some(format!("0_{name}")),
                    ..Default::default()
                });
            }
        }
    }

    if let (Some(dir), Some(root)) = (paths.dir, paths.project()) {
        let imports = crate::modules::use_imports(text);
        let mut imported = crate::modules::type_members(
            dir,
            root,
            paths.std,
            paths.packages(),
            &imports,
            &type_name,
        );
        imported.sort_by(|a, b| a.name.cmp(&b.name));

        for member in imported {
            if member.kind != ModuleKind::Field
                || used.contains(&member.name)
                || !member.name.starts_with(&filter)
            {
                continue;
            }

            if seen.insert(member.name.clone()) {
                items.push(CompletionItem {
                    label: member.name.clone(),
                    kind: Some(CompletionItemKind::FIELD),
                    detail: Some(member.module.clone()),
                    sort_text: Some(format!("0_{}", member.name)),
                    ..Default::default()
                });
            }
        }

        if items.is_empty() {
            let mut payload = crate::modules::variant_payload_fields(
                dir,
                root,
                paths.std,
                paths.packages(),
                &imports,
                &type_name,
            );
            payload.sort_by(|a, b| a.name.cmp(&b.name));

            for member in payload {
                if used.contains(&member.name) || !member.name.starts_with(&filter) {
                    continue;
                }

                if seen.insert(member.name.clone()) {
                    items.push(CompletionItem {
                        label: member.name.clone(),
                        kind: Some(CompletionItemKind::FIELD),
                        detail: Some(member.module.clone()),
                        sort_text: Some(format!("0_{}", member.name)),
                        ..Default::default()
                    });
                }
            }
        }
    }

    items
}

pub fn repair_for_completion(text: &str, offset: usize) -> Option<String> {
    repair_impl(text, offset, 0, false)
}

pub fn repair_for_completion_closed(text: &str, offset: usize) -> Option<String> {
    repair_impl(text, offset, 1, false)
}

pub fn repair_for_completion_nested(text: &str, offset: usize, extra: usize) -> Option<String> {
    repair_impl(text, offset, extra.min(3), false)
}

pub fn repair_for_completion_semi(
    text: &str,
    offset: usize,
    extra: usize,
    semi: bool,
) -> Option<String> {
    repair_impl(text, offset, extra.min(3), semi)
}

fn repair_impl(text: &str, offset: usize, extra_closes: usize, semi: bool) -> Option<String> {
    let offset = offset.min(text.len());
    let bytes = text.as_bytes();

    if offset == 0 || bytes.get(offset - 1) != Some(&b'.') {
        return None;
    }

    if offset < bytes.len() {
        let next = bytes[offset];

        if next.is_ascii_alphanumeric() || next == b'_' {
            return None;
        }
    }

    if offset >= 2 && bytes[offset - 2].is_ascii_digit() {
        return None;
    }

    let in_field = struct_init_field_position(text, offset);

    if in_field {
        let mut repaired = String::with_capacity(text.len() + 24);
        repaired.push_str(&text[..offset]);
        repaired.push_str("__zeen_complete = 0");

        if extra_closes > 0 {
            for _ in 0..extra_closes {
                repaired.push('}');
            }

            if semi {
                repaired.push(';');
            } else {
                let mut rest = offset;

                while rest < bytes.len() && bytes[rest].is_ascii_whitespace() {
                    rest += 1;
                }

                if !matches!(
                    bytes.get(rest),
                    Some(b';') | Some(b',') | Some(b')') | Some(b']') | Some(b'}')
                ) {
                    repaired.push(';');
                }
            }
        }

        repaired.push_str(&text[offset..]);

        let mut balance: isize = 0;

        for byte in repaired.bytes() {
            match byte {
                b'{' => balance += 1,
                b'}' => balance -= 1,
                _ => {}
            }
        }

        for _ in 0..balance {
            repaired.push('}');
        }

        return Some(repaired);
    }

    let mut repaired = String::with_capacity(text.len() + 1);
    repaired.push_str(&text[..offset - 1]);

    let mut rest = offset;

    while rest < bytes.len() && bytes[rest].is_ascii_whitespace() {
        rest += 1;
    }

    let need_semi = !matches!(
        bytes.get(rest),
        None | Some(b';') | Some(b')') | Some(b']') | Some(b',')
    );

    if rest == bytes.len() || need_semi {
        repaired.push(';');
    }

    repaired.push_str(&text[offset..]);

    Some(repaired)
}

pub fn receiver_dot_before(text: &str, offset: usize) -> Option<usize> {
    let offset = offset.min(text.len());
    let bytes = text.as_bytes();
    let mut end = offset;

    while end > 0 && (bytes[end - 1].is_ascii_alphanumeric() || bytes[end - 1] == b'_') {
        end -= 1;
    }

    if end == 0 || bytes[end - 1] != b'.' {
        return None;
    }

    let dot = end - 1;
    let mut start = dot;

    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }

    if start == dot {
        return None;
    }

    Some(dot)
}

pub fn struct_init_field_position(text: &str, offset: usize) -> bool {
    if struct_init_context(text, offset).is_none() {
        return false;
    }

    let offset = offset.min(text.len());
    let bytes = text.as_bytes();
    let mut end = offset;

    while end > 0 && (bytes[end - 1].is_ascii_alphanumeric() || bytes[end - 1] == b'_') {
        end -= 1;
    }

    if end != offset {
        return end > 0 && bytes[end - 1] == b'.';
    }

    let mut cursor = end;

    while cursor > 0 && bytes[cursor - 1].is_ascii_whitespace() {
        cursor -= 1;
    }

    if cursor == 0 {
        return false;
    }

    match bytes[cursor - 1] {
        b'{' | b',' => true,
        b'.' => {
            if cursor < 2 {
                return true;
            }

            let prev = bytes[cursor - 2];

            !(prev.is_ascii_alphanumeric() || prev == b'_' || prev == b')' || prev == b']')
        }
        _ => false,
    }
}

pub fn use_path_prefix(text: &str, offset: usize) -> Option<String> {
    let position = crate::position::offset_to_position(text, offset.min(text.len()));
    let line = crate::position::line_text(text, position.line);
    let upto = (position.character as usize).min(line.len());
    let before: String = line.chars().take(upto).collect();
    let trimmed = before.trim_start();

    let rest = trimmed
        .strip_prefix("use ")
        .or_else(|| trimmed.strip_prefix("use\t"))?;

    Some(rest.trim_start().to_string())
}

pub fn complete_use(prefix: &str, modules: &[String]) -> Vec<CompletionItem> {
    let mut items = Vec::new();

    for module in modules {
        let matches = module.starts_with(prefix)
            || (!prefix.contains('.')
                && module
                    .rsplit('.')
                    .next()
                    .is_some_and(|last| last.starts_with(prefix)));

        if matches {
            items.push(CompletionItem {
                label: module.clone(),
                kind: Some(CompletionItemKind::MODULE),
                ..Default::default()
            });
        }
    }

    items.sort_by(|a, b| a.label.cmp(&b.label));
    items
}

pub fn core_modules() -> Vec<String> {
    zeen_driver::CORE_FILES
        .iter()
        .map(|file| file.name.to_string())
        .collect()
}

pub fn std_modules(root: &std::path::Path) -> Vec<String> {
    let mut modules = Vec::new();

    collect_std_modules(root, root, &mut modules);
    modules.sort();
    modules
}

fn collect_std_modules(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<String>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'))
        {
            continue;
        }

        if path.is_dir() {
            collect_std_modules(root, &path, out);
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) != Some("zn") {
            continue;
        }

        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };

        let mut name = String::from("std");

        for component in relative.components() {
            let Some(text) = component.as_os_str().to_str() else {
                continue;
            };

            name.push('.');
            name.push_str(text);
        }

        if let Some(dotted) = name.strip_suffix(".zn") {
            out.push(dotted.to_string());
        }
    }
}

pub fn sibling_modules(dir: &std::path::Path, open_name: &str) -> Vec<String> {
    let mut modules = Vec::new();

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();

            if path.extension().and_then(|ext| ext.to_str()) != Some("zn") {
                continue;
            }

            if let Some(stem) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| *stem != open_name)
            {
                modules.push(stem.to_string());
            }
        }
    }

    modules.sort();
    modules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_pointers_and_const() {
        assert_eq!(normalize_type_name("Point"), Some("Point".to_string()));
        assert_eq!(normalize_type_name("*Point"), Some("Point".to_string()));
        assert_eq!(
            normalize_type_name("*const Point"),
            Some("Point".to_string())
        );
        assert_eq!(normalize_type_name("List[i32]"), Some("List".to_string()));
        assert_eq!(normalize_type_name("[5]i32"), None);
        assert_eq!(normalize_type_name("fn(i32) void"), None);
        assert_eq!(normalize_type_name("Fn(i32) i32"), None);
    }

    #[test]
    fn use_prefix_matches_dotted_paths() {
        let modules = [
            "super.lib".to_string(),
            "super.other".to_string(),
            "root.lib".to_string(),
        ];
        let items = complete_use("super.li", &modules);
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();

        assert_eq!(labels, vec!["super.lib"]);
    }

    #[test]
    fn receiver_dot_detects_prefix() {
        assert_eq!(receiver_dot_before("p.", 2), Some(1));
        assert_eq!(receiver_dot_before("p.le", 4), Some(1));
        assert_eq!(receiver_dot_before("Point.new", 9), Some(5));
        assert_eq!(receiver_dot_before("p", 1), None);
        assert_eq!(receiver_dot_before("p + q", 5), None);
        assert_eq!(receiver_dot_before(".", 1), None);
    }

    #[test]
    fn call_dot_detects_callee() {
        let text = "let s = Outer.spawn().inner;";
        let dot = text.find("().").unwrap() + 2;
        let trigger = call_dot_before(text, dot + 1).unwrap();
        assert_eq!(dot, trigger.dot);
        assert_eq!(
            &text[trigger.callee.0..trigger.callee.0 + trigger.callee.1],
            "Outer.spawn"
        );

        let nested = "let s = foo().bar().baz;";
        let dot = nested.find("().baz").unwrap() + 2;
        let trigger = call_dot_before(nested, dot + 1).unwrap();
        assert_eq!(
            &nested[trigger.callee.0..trigger.callee.0 + trigger.callee.1],
            "foo().bar"
        );

        assert!(call_dot_before("p.len", 5).is_none());
        assert!(call_dot_before("p.", 2).is_none());
    }

    #[test]
    fn struct_init_context_finds_owner() {
        let text = "let p = Point { .x = 1, .";
        let offset = text.len();
        let (name, _) = struct_init_context(text, offset).unwrap();
        assert_eq!(name, "Point");

        let body = "fn f() { x";
        assert!(struct_init_context(body, body.len()).is_none());
    }

    #[test]
    fn struct_init_position_separates_value() {
        let field = "let p = Point { .";
        assert!(struct_init_field_position(field, field.len()));

        let partial = "let p = Point { .x = 1, .y";
        assert!(struct_init_field_position(partial, partial.len()));

        let value = "let p = Point { .x = ";
        assert!(!struct_init_field_position(value, value.len()));

        let nested = "let p = Point { .x = foo.";
        assert!(!struct_init_field_position(nested, nested.len()));
        assert_eq!(
            receiver_dot_before(nested, nested.len()),
            Some(nested.len() - 1)
        );
    }

    #[test]
    fn repair_deletes_dangling_dot() {
        let text = "let q = p.";
        let repaired = repair_for_completion(text, text.len()).unwrap();
        assert_eq!(repaired, "let q = p;");

        let done = "let q = p.len";
        assert!(repair_for_completion(done, done.len()).is_none());

        let strukt = "let p = Point { .";
        let repaired = repair_for_completion(strukt, strukt.len()).unwrap();
        assert!(repaired.contains(".__zeen_complete = 0"));
        assert!(repaired.ends_with('}'));
    }

    const SAMPLE: &str = "struct Point {\n    pub x: i32,\n    pub y: i32,\n\n    pub fn new(x: i32, y: i32) Point {\n        return Point { .x = x, .y = y };\n    }\n\n    pub fn len(*self) i32 {\n        return self.x;\n    }\n}\n\nfn main() {\n    let p = Point.new(1, 2);\n    let q = p.len();\n    @println(\"{} {}\", p.x, q);\n}\n";

    fn sample_analysis(text: &str) -> crate::analysis::Analysis {
        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-probe.zn".parse().unwrap();
        let output = crate::diagnostics::check(&uri, text, &[]);
        assert!(
            output.diagnostics.is_empty(),
            "sample must check cleanly: {:?}",
            output.diagnostics
        );
        output.analysis
    }

    fn labels(items: &[CompletionItem]) -> Vec<&str> {
        items.iter().map(|item| item.label.as_str()).collect()
    }

    #[test]
    fn static_dot_shows_only_static() {
        let analysis = sample_analysis(SAMPLE);
        let dot = SAMPLE.find("Point.new").unwrap() + "Point".len();
        let items = dot_complete(SAMPLE, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"new"));

        for hidden in ["len", "x", "y"] {
            assert!(!names.contains(&hidden), "static must hide {hidden}");
        }

        let new_item = items.iter().find(|item| item.label == "new").unwrap();
        assert_eq!(new_item.kind, Some(CompletionItemKind::FUNCTION));
    }

    #[test]
    fn value_dot_shows_methods_and_fields() {
        let analysis = sample_analysis(SAMPLE);
        let dot = SAMPLE.find("p.len").unwrap() + "p".len();
        let items = dot_complete(SAMPLE, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["len"]);

        let len_item = items.iter().find(|item| item.label == "len").unwrap();
        assert_eq!(len_item.kind, Some(CompletionItemKind::METHOD));
    }

    #[test]
    fn dangling_dot_recovers_through_repair() {
        let broken = SAMPLE.replacen(
            "    let p = Point.new(1, 2);",
            "    let p = Point.new(1, 2);\n    p.",
            1,
        );
        let offset = broken.find("\n    p.").unwrap() + "\n    p.".len();
        assert_eq!(receiver_dot_before(&broken, offset), Some(offset - 1));

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-probe.zn".parse().unwrap();
        let stale = crate::diagnostics::check(&uri, &broken, &[]);
        assert!(
            dot_complete(
                &broken,
                &stale.analysis,
                offset - 1,
                ResolvePaths::default()
            )
            .is_empty()
        );

        let repaired = repair_for_completion(&broken, offset).unwrap();
        let fresh = crate::diagnostics::check(&uri, &repaired, &[]);
        for (def, members) in &fresh.analysis.members {
            eprintln!("MEMBERS {def:?} count {}", members.len());
        }
        let items = dot_complete(
            &broken,
            &fresh.analysis,
            offset - 1,
            ResolvePaths::default(),
        );
        let names = labels(&items);

        assert!(names.contains(&"len"));
        assert!(names.contains(&"x"));
        assert!(!names.contains(&"new"));

        let len_item = items.iter().find(|item| item.label == "len").unwrap();
        assert_eq!(len_item.kind, Some(CompletionItemKind::METHOD));

        let field_item = items.iter().find(|item| item.label == "x").unwrap();
        assert_eq!(field_item.kind, Some(CompletionItemKind::FIELD));
    }

    #[test]
    fn struct_init_suggests_missing_fields() {
        let text = SAMPLE.replacen(
            "    let p = Point.new(1, 2);",
            "    let p = Point.new(1, 2);\n    let r = Point { .x = 1, .",
            1,
        );
        let anchor = "Point { .x = 1, .";
        let offset = text.find(anchor).unwrap() + anchor.len();

        assert!(struct_init_field_position(&text, offset));

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-probe.zn".parse().unwrap();
        let first = repair_for_completion(&text, offset).unwrap();
        let mut output = crate::diagnostics::check(&uri, &first, &[]);

        if output.analysis.occurrences.is_empty() {
            let second = repair_for_completion_closed(&text, offset).unwrap();
            let retry = crate::diagnostics::check(&uri, &second, &[]);

            if !retry.analysis.occurrences.is_empty() {
                output = retry;
            }
        }

        let items = struct_init_complete(&text, &output.analysis, offset, ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["y"]);
    }

    #[test]
    fn struct_init_with_closer_below() {
        let text = SAMPLE.replacen(
            "    let p = Point.new(1, 2);",
            "    let p = Point.new(1, 2);\n    let r = Point { .x = 1, .\n    };",
            1,
        );
        let anchor = "Point { .x = 1, .";
        let offset = text.find(anchor).unwrap() + anchor.len();

        assert!(struct_init_field_position(&text, offset));

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-probe.zn".parse().unwrap();
        let repaired = repair_for_completion(&text, offset).unwrap();
        let output = crate::diagnostics::check(&uri, &repaired, &[]);
        assert!(!output.analysis.occurrences.is_empty());

        let items = struct_init_complete(&text, &output.analysis, offset, ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["y"]);
    }

    const IMPORT_LIB: &str = "pub struct Helper {\n    pub x: i32,\n\n    pub fn make() Helper {\n        return Helper { .x = 1 };\n    }\n\n    pub fn get(*self) i32 {\n        return self.x;\n    }\n}\n";

    fn variant_sample() -> String {
        "enum Widget {\n    empty,\n    point: {\n        x: i32,\n        y: i32,\n    },\n}\n\nfn main() {\n    let m = Widget.point { .x = 1, .\n    @println(\"{}\", @enumTag(m));\n}\n"
            .to_string()
    }

    fn check_first_working(
        uri: &tower_lsp_server::ls_types::Uri,
        text: &str,
        offset: usize,
    ) -> crate::analysis::Analysis {
        let base = repair_for_completion(text, offset).unwrap();
        let mut last = crate::diagnostics::check(uri, &base, &[]);
        let mut seen = base;

        for extra in 0..=3 {
            for semi in [false, true] {
                if last.analysis.occurrences.is_empty()
                    && let Some(next) = repair_for_completion_semi(text, offset, extra, semi)
                    && next != seen
                {
                    seen = next.clone();
                    let retry = crate::diagnostics::check(uri, &next, &[]);

                    if !retry.analysis.occurrences.is_empty() {
                        last = retry;
                    }
                }

                if !last.analysis.occurrences.is_empty() {
                    break;
                }
            }
        }

        last.analysis
    }

    #[test]
    fn variant_payload_suggests_missing_fields() {
        let text = variant_sample();
        let anchor = "Widget.point { .x = 1, .";
        let offset = text.find(anchor).unwrap() + anchor.len();

        assert!(struct_init_field_position(&text, offset));

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-probe.zn".parse().unwrap();
        let analysis = check_first_working(&uri, &text, offset);
        let items = struct_init_complete(&text, &analysis, offset, ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["y"]);
    }

    const NESTED_SAMPLE: &str = "struct Inner {\n    pub a: i32,\n    pub c: i32,\n}\n\nstruct Outer {\n    pub inner: Inner,\n    pub b: i32,\n}\n\nfn main() {\n    let o = Outer { .b = 2, .inner = Inner { .a = 1, .";

    #[test]
    fn nested_init_repairs_two_levels() {
        let offset = NESTED_SAMPLE.len();

        assert!(struct_init_field_position(NESTED_SAMPLE, offset));

        let (name, _) = struct_init_context(NESTED_SAMPLE, offset).unwrap();
        assert_eq!(name, "Inner");

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-probe.zn".parse().unwrap();
        let analysis = check_first_working(&uri, NESTED_SAMPLE, offset);
        let items = struct_init_complete(NESTED_SAMPLE, &analysis, offset, ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["c"]);
    }

    fn import_project(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("zeen-lsp-imports-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join("pkg")).unwrap();
        std::fs::write(dir.join("main.zn"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("lib.zn"), IMPORT_LIB).unwrap();
        std::fs::write(dir.join("sub").join("lib.zn"), IMPORT_LIB).unwrap();
        std::fs::write(
            dir.join("pkg").join("index.zn"),
            IMPORT_LIB.replace("Helper", "Indexed"),
        )
        .unwrap();
        dir
    }

    #[test]
    fn dotted_super_root_and_index_imports() {
        let dir = import_project("dotted");
        let sub = dir.join("sub");

        let helper = |members: Vec<crate::modules::ModuleMember>| {
            let names: Vec<&str> = members.iter().map(|member| member.name.as_str()).collect();
            assert!(names.contains(&"x"));
            assert!(names.contains(&"make"));
            assert!(names.contains(&"get"));

            let make = members.iter().find(|member| member.name == "make").unwrap();
            assert!(make.is_static);

            let get = members.iter().find(|member| member.name == "get").unwrap();
            assert!(!get.is_static);
        };

        helper(crate::modules::type_members(
            &sub,
            &dir,
            None,
            &[],
            &["super.lib".to_string()],
            "Helper",
        ));

        helper(crate::modules::type_members(
            &sub,
            &dir,
            None,
            &[],
            &["root.lib".to_string()],
            "Helper",
        ));

        helper(crate::modules::type_members(
            &dir,
            &dir,
            None,
            &[],
            &["sub.lib".to_string()],
            "Helper",
        ));

        let indexed =
            crate::modules::type_members(&dir, &dir, None, &[], &["pkg".to_string()], "Indexed");
        assert!(indexed.iter().any(|member| member.name == "x"));

        let through_package = crate::modules::members_of_use(
            &dir,
            &dir,
            None,
            &[("http".to_string(), dir.join("pkg"))],
            "http",
        );
        assert!(
            through_package
                .iter()
                .any(|member| member.name == "Indexed")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    const PRIVATE_SAMPLE: &str = "struct Vault {\n    pub opened: i32,\n    secret: i32,\n\n    pub fn make(o: i32, s: i32) Vault {\n        return Vault { .opened = o, .secret = s };\n    }\n\n    pub fn open(*self) i32 {\n        return self.secret;\n    }\n\n    fn poke(*self) i32 {\n        return self.secret;\n    }\n\n    fn combo() i32 {\n        return 7;\n    }\n}\n\nimplement Clone : Vault {\n    pub fn clone(*const self) Vault {\n        return Vault { .opened = 1, .secret = 2 };\n    }\n}\n\nfn main() {\n    let v = Vault.make(1, 2);\n    let o = v.open();\n    @println(\"{} {}\", v.opened, o);\n}\n";

    const CHAIN_SAMPLE: &str = "struct Inner {\n    pub v: i32,\n\n    pub fn get(*self) i32 {\n        return self.v;\n    }\n}\n\nstruct Outer {\n    pub inner: Inner,\n\n    pub fn spawn() Outer {\n        return Outer { .inner = Inner { .v = 1 } };\n    }\n}\n\nfn main() {\n    let o = Outer.spawn();\n    let g = o.inner.get();\n    let s = Outer.spawn().inner;\n    @println(\"{}\", o.inner.v);\n    @println(\"{}\", g);\n    @println(\"{}\", s.v);\n}\n";

    #[test]
    fn field_chain_resolves_middle_type() {
        let analysis = sample_analysis(CHAIN_SAMPLE);

        let dot = CHAIN_SAMPLE.find("o.inner.get").unwrap() + "o.inner".len();
        let items = dot_complete(CHAIN_SAMPLE, &analysis, dot, ResolvePaths::default());
        assert_eq!(labels(&items), vec!["get"]);

        let dot = CHAIN_SAMPLE.find("o.inner.v").unwrap() + "o.inner".len();
        let items = dot_complete(CHAIN_SAMPLE, &analysis, dot, ResolvePaths::default());
        assert_eq!(labels(&items), vec!["v"]);
    }

    #[test]
    fn call_chain_resolves_result_type() {
        let analysis = sample_analysis(CHAIN_SAMPLE);
        let anchor = "Outer.spawn().inner";
        let dot = CHAIN_SAMPLE.find(anchor).unwrap() + "Outer.spawn()".len();

        let trigger = call_dot_before(CHAIN_SAMPLE, dot + 1).unwrap();
        assert_eq!(trigger.dot, dot);

        let items = call_complete(CHAIN_SAMPLE, &analysis, trigger, ResolvePaths::default());
        assert_eq!(labels(&items), vec!["inner"]);
    }

    #[test]
    fn dangling_call_recovers_through_repair() {
        let broken = CHAIN_SAMPLE.replacen(
            "    let s = Outer.spawn().inner;",
            "    let s = Outer.spawn().inner;\n    Outer.spawn().",
            1,
        );
        let offset = broken.find("\n    Outer.spawn().").unwrap() + "\n    Outer.spawn().".len();

        let trigger = call_dot_before(&broken, offset).unwrap();

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-probe.zn".parse().unwrap();
        let analysis = check_first_working(&uri, &broken, offset);
        let items = call_complete(&broken, &analysis, trigger, ResolvePaths::default());
        assert_eq!(labels(&items), vec!["inner"]);
    }

    #[test]
    fn dangling_chain_recovers_through_repair() {
        let broken = CHAIN_SAMPLE.replacen(
            "    let g = o.inner.get();",
            "    let g = o.inner.get();\n    o.inner.",
            1,
        );
        let offset = broken.find("\n    o.inner.").unwrap() + "\n    o.inner.".len();

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-probe.zn".parse().unwrap();
        let analysis = check_first_working(&uri, &broken, offset);
        let dot = offset - 1;
        let items = dot_complete(&broken, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"get"));
        assert!(names.contains(&"v"));
    }

    #[test]
    fn private_hidden_outside() {
        let analysis = sample_analysis(PRIVATE_SAMPLE);

        let value = PRIVATE_SAMPLE.find("v.opened").unwrap() + "v".len();
        let items = dot_complete(PRIVATE_SAMPLE, &analysis, value, ResolvePaths::default());
        let names = labels(&items);
        assert_eq!(names, vec!["opened"]);

        let statik = PRIVATE_SAMPLE.find("Vault.make").unwrap() + "Vault".len();
        let items = dot_complete(PRIVATE_SAMPLE, &analysis, statik, ResolvePaths::default());
        let names = labels(&items);
        assert_eq!(names, vec!["make"]);
    }

    #[test]
    fn private_shown_inside_own_method() {
        let analysis = sample_analysis(PRIVATE_SAMPLE);

        let dot = PRIVATE_SAMPLE.find("self.secret").unwrap() + "self".len();
        let items = dot_complete(PRIVATE_SAMPLE, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"secret"));
    }

    #[test]
    fn private_shown_inside_implement() {
        let text = PRIVATE_SAMPLE.replacen(
            "return Vault { .opened = 1, .secret = 2 };",
            "return Vault { .opened = 1, .",
            1,
        );
        let anchor = "Vault { .opened = 1, .";
        let offset = text.find(anchor).unwrap() + anchor.len();

        assert!(struct_init_field_position(&text, offset));

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-probe.zn".parse().unwrap();
        let analysis = check_first_working(&uri, &text, offset);
        let items = struct_init_complete(&text, &analysis, offset, ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["secret"]);
    }

    #[test]
    fn global_complete_hides_private_outside() {
        let analysis = sample_analysis(PRIVATE_SAMPLE);

        let text = format!("{PRIVATE_SAMPLE}\n    comb");
        let items = complete(&text, &analysis, text.len(), ResolvePaths::default());
        let names = labels(&items);
        assert!(!names.contains(&"combo"));

        let text = format!("{PRIVATE_SAMPLE}\n    mak");
        let items = complete(&text, &analysis, text.len(), ResolvePaths::default());
        let names = labels(&items);
        assert!(names.contains(&"make"));
    }

    #[test]
    fn package_types_complete_through_deps_map() {
        let dir = std::env::temp_dir().join(format!("zeen-lsp-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pkg = dir.join("httplib");
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            dir.join("Zora.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nzeen = \"0.3\"\ntype = \"bin\"\n",
        )
        .unwrap();
        std::fs::write(
            pkg.join("index.zn"),
            "pub struct Client {\n    pub base: i32,\n\n    pub fn make() Client {\n        return Client { .base = 1 };\n    }\n\n    pub fn get(*self) i32 {\n        return self.base;\n    }\n}\n",
        )
        .unwrap();

        let main = dir.join("src").join("main.zn");
        let text = "use http;\n\nfn main() {\n    let c = Client.make();\n    @println(\"{}\", c.base);\n}\n";
        std::fs::write(&main, text).unwrap();

        let uri: tower_lsp_server::ls_types::Uri =
            format!("file://{}", main.display()).parse().unwrap();
        let packages = vec![("http".to_string(), pkg)];

        assert_eq!(crate::zora::find_root(&main), Some(dir.clone()));
        assert_eq!(
            crate::zora::entry_for_root(&dir),
            Some(dir.join("src").join("main.zn"))
        );

        let output = crate::diagnostics::check(&uri, text, &packages);
        assert!(
            output.diagnostics.is_empty(),
            "package sample must check cleanly: {:?}",
            output.diagnostics
        );

        let src = dir.join("src");
        let paths = ResolvePaths {
            dir: Some(src.as_path()),
            root: Some(dir.as_path()),
            std: None,
            packages: Some(packages.as_slice()),
        };

        let dot = text.find("Client.make").unwrap() + "Client".len();
        let items = dot_complete(text, &output.analysis, dot, paths);
        let names = labels(&items);
        assert!(names.contains(&"make"));
        assert!(!names.contains(&"get"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn project_self_mapping_resolves_use() {
        let dir = std::env::temp_dir().join(format!("zeen-lsp-selfpkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            dir.join("Zora.toml"),
            "[package]\nname = \"adder\"\nversion = \"0.1.0\"\nzeen = \"0.3\"\ntype = \"bin\"\n",
        )
        .unwrap();
        std::fs::write(
            src.join("calc.zn"),
            "pub fn add(a: i32, b: i32) i32 {\n    return a + b;\n}\n",
        )
        .unwrap();

        let main = src.join("main.zn");
        let text = "use adder.calc;\n\nfn main() {\n    @println(\"{}\", add(1, 2));\n}\n";
        std::fs::write(&main, text).unwrap();

        let uri: tower_lsp_server::ls_types::Uri =
            format!("file://{}", main.display()).parse().unwrap();
        let mapping = crate::zora::self_mapping(&dir).unwrap();
        assert_eq!(mapping.0, "adder");

        let packages = vec![mapping];
        let output = crate::diagnostics::check(&uri, text, &packages);
        assert!(
            output.diagnostics.is_empty(),
            "self import must check cleanly: {:?}",
            output.diagnostics
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn imported_variant_payload_suggests_fields() {
        let dir = import_project("variant");
        std::fs::write(
            dir.join("sub").join("sig.zn"),
            "pub enum Signal {\n    stop,\n    go: {\n        speed: i32,\n        dir: i32,\n    },\n}\n",
        )
        .unwrap();

        let text = "use sub.sig;\n\nfn main() {\n    let s = go { .speed = 1, .";
        let anchor = "go { .speed = 1, .";
        let offset = text.find(anchor).unwrap() + anchor.len();

        assert!(struct_init_field_position(text, offset));

        let paths = ResolvePaths {
            dir: Some(dir.as_path()),
            root: Some(dir.as_path()),
            std: None,
            packages: None,
        };
        let items = struct_init_complete(text, &Analysis::default(), offset, paths);
        let names = labels(&items);

        assert_eq!(names, vec!["dir"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
