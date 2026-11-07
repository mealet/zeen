use std::collections::HashSet;

use tower_lsp_server::ls_types::{
    CompletionItem, CompletionItemKind, CompletionItemLabelDetails, InsertTextFormat,
};
use zeen_ast::types::BuiltinShape;
use zeen_hir::HirMacroKind;
use zeen_lexer::token::{CompilerKeyword, CompilerType};

use crate::analysis::{Analysis, BuiltinTarget, Member, MemberKind, Role, TypeKind};
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

fn is_at_trigger(text: &str, offset: usize, prefix: &str) -> bool {
    let start = offset.min(text.len()).saturating_sub(prefix.len());

    start > 0 && text.as_bytes().get(start - 1) == Some(&b'@')
}

fn bracket_directive(text: &str, offset: usize, prefix: &str) -> Option<(String, String)> {
    let offset = offset.min(text.len());
    let bytes = text.as_bytes();
    let mut cursor = offset.saturating_sub(prefix.len());

    if cursor == 0 || bytes[cursor - 1] != b'[' {
        return None;
    }

    cursor -= 1;
    let mut start = cursor;

    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }

    if start == cursor || start == 0 || bytes[start - 1] != b'@' {
        return None;
    }

    Some((text.get(start..cursor)?.to_string(), prefix.to_string()))
}

const PREPROC_DIRECTIVES: &[&str] = &[
    "os", "arch", "env", "target", "family", "var", "debug", "release",
];

fn directive_values(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "os" => Some(&["linux", "windows", "darwin", "freebsd", "android", "wasip1"]),
        "arch" => Some(&["x86_64", "aarch64", "i686", "wasm32", "wasm64"]),
        "env" => Some(&["gnu", "musl", "msvc", "android"]),
        "family" => Some(&["unix", "windows", "wasm"]),
        "var" => Some(&["os", "arch", "env", "target", "family", "debug", "release"]),
        "target" => Some(&[
            "x86_64-unknown-linux-gnu",
            "x86_64-unknown-linux-musl",
            "i686-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
            "x86_64-pc-windows-gnu",
            "x86_64-pc-windows-msvc",
            "i686-pc-windows-gnu",
            "i686-pc-windows-msvc",
            "wasm32-unknown-unknown",
            "wasm32-unknown-wasip1",
            "x86_64-unknown-freebsd",
            "aarch64-linux-android",
        ]),
        _ => None,
    }
}

fn complete_directive_values(directive: &str, prefix: &str) -> Vec<CompletionItem> {
    let Some(values) = directive_values(directive) else {
        return Vec::new();
    };

    values
        .iter()
        .filter(|value| value.starts_with(prefix))
        .map(|value| CompletionItem {
            label: value.to_string(),
            kind: Some(CompletionItemKind::VALUE),
            ..Default::default()
        })
        .collect()
}

fn complete_at_items(prefix: &str) -> Vec<CompletionItem> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut items = Vec::new();

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

    for directive in PREPROC_DIRECTIVES {
        let label = format!("@{directive}");

        if directive.starts_with(prefix) && seen.insert(label.clone()) {
            items.push(CompletionItem {
                label,
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }
    }

    items
}

pub fn complete(
    text: &str,
    analysis: &Analysis,
    offset: usize,
    paths: ResolvePaths<'_>,
) -> Vec<CompletionItem> {
    let prefix = ident_prefix(text, offset);

    if let Some((directive, value_prefix)) = bracket_directive(text, offset, prefix) {
        return complete_directive_values(&directive, &value_prefix);
    }

    if is_at_trigger(text, offset, prefix) {
        return complete_at_items(prefix);
    }

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

        if matches!(occurrence.role, Role::Variable | Role::Parameter)
            && let Some((start, end)) = occurrence.scope
            && !(start <= offset && offset <= end)
        {
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

    for member in crate::modules::core_members() {
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

    if let (Some(dir), Some(root)) = (paths.dir, paths.project()) {
        for import in crate::modules::imports_for(text, paths.std) {
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

enum FrameKind {
    Implement(String),
    Other,
}

struct Frame {
    kind: FrameKind,
    body_start: usize,
}

fn skip_string(bytes: &[u8], mut cursor: usize) -> usize {
    cursor += 1;

    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor += 2,
            b'"' => return cursor + 1,
            _ => cursor += 1,
        }
    }

    bytes.len()
}

fn skip_line(bytes: &[u8], mut cursor: usize) -> usize {
    while cursor < bytes.len() && bytes[cursor] != b'\n' {
        cursor += 1;
    }

    cursor
}

fn skip_block_comment(bytes: &[u8], mut cursor: usize) -> usize {
    cursor += 2;

    while cursor + 1 < bytes.len() {
        if bytes[cursor] == b'*' && bytes[cursor + 1] == b'/' {
            return cursor + 2;
        }

        cursor += 1;
    }

    bytes.len()
}

fn implement_name(header: &str) -> Option<String> {
    let rest = header.trim_start().strip_prefix("implement")?;

    if !rest.is_empty() && !rest.starts_with([' ', '\t', '[']) {
        return None;
    }

    let rest = rest.trim_start();
    let rest = match rest.strip_prefix('[') {
        Some(after) => after.get(after.find(']')? + 1..)?.trim_start(),
        None => rest,
    };

    let name: String = rest
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect();

    (!name.is_empty()).then_some(name)
}

fn scan_frames(text: &str, upto: usize) -> Vec<Frame> {
    let bytes = text.as_bytes();
    let limit = upto.min(bytes.len());
    let mut frames = Vec::new();
    let mut segment = 0usize;
    let mut cursor = 0usize;

    while cursor < limit {
        match bytes[cursor] {
            b'"' => cursor = skip_string(bytes, cursor),
            b'/' if bytes.get(cursor + 1) == Some(&b'/') => cursor = skip_line(bytes, cursor),
            b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                cursor = skip_block_comment(bytes, cursor)
            }
            b'{' => {
                let header = text.get(segment..cursor).unwrap_or("");
                let kind = implement_name(header)
                    .map(FrameKind::Implement)
                    .unwrap_or(FrameKind::Other);

                frames.push(Frame {
                    kind,
                    body_start: cursor + 1,
                });
                segment = cursor + 1;
                cursor += 1;
            }
            b'}' => {
                frames.pop();
                segment = cursor + 1;
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }

    frames
}

fn has_fn_keyword(text: &str, offset: usize, typed: &str) -> bool {
    let end = offset.min(text.len()).saturating_sub(typed.len());
    let Some(before) = text.get(..end) else {
        return false;
    };
    let trimmed = before.trim_end_matches([' ', '\t']);
    let Some(rest) = trimmed.strip_suffix("fn") else {
        return false;
    };

    rest.is_empty()
        || !rest.ends_with(|character: char| character.is_ascii_alphanumeric() || character == '_')
}

fn implemented_methods(text: &str, body_start: usize) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut names = Vec::new();
    let mut depth = 0i32;
    let mut cursor = body_start;

    while cursor < bytes.len() {
        match bytes[cursor] {
            b'"' => cursor = skip_string(bytes, cursor),
            b'/' if bytes.get(cursor + 1) == Some(&b'/') => cursor = skip_line(bytes, cursor),
            b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                cursor = skip_block_comment(bytes, cursor)
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;

                if depth < 0 {
                    break;
                }
            }
            b'f' if depth == 0 => {
                let boundary = cursor == 0
                    || !(bytes[cursor - 1].is_ascii_alphanumeric() || bytes[cursor - 1] == b'_');

                if boundary
                    && let Some(after) = text.get(cursor + 2..)
                    && text[cursor..].starts_with("fn")
                    && after.starts_with([' ', '\t'])
                {
                    let name: String = after
                        .trim_start()
                        .chars()
                        .take_while(|character| {
                            character.is_ascii_alphanumeric() || *character == '_'
                        })
                        .collect();

                    if !name.is_empty() {
                        names.push(name);
                    }
                }
            }
            _ => {}
        }

        cursor += 1;
    }

    names
}

fn alias_decl_prefix(text: &str, offset: usize) -> Option<&str> {
    let offset = offset.min(text.len());
    let line_start = text[..offset]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let line = text.get(line_start..offset)?;
    let rest = line.trim_start().strip_prefix("alias")?;

    if !rest.starts_with([' ', '\t']) {
        return None;
    }

    let rest = rest.trim_start();

    if rest.is_empty() {
        return Some("");
    }

    rest.bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        .then_some(rest)
}

fn implemented_aliases(text: &str, body_start: usize, limit: usize) -> Vec<String> {
    let Some(body) = text.get(body_start..limit.min(text.len())) else {
        return Vec::new();
    };

    let mut names = Vec::new();

    for line in body.lines() {
        let Some(rest) = line.trim_start().strip_prefix("alias") else {
            continue;
        };

        if !rest.starts_with([' ', '\t']) {
            continue;
        }

        let name: String = rest
            .trim_start()
            .chars()
            .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
            .collect();

        if !name.is_empty() {
            names.push(name);
        }
    }

    names
}

fn implement_header_name(header: &str) -> Option<&str> {
    let rest = header.trim_start().strip_prefix("implement")?;

    if !rest.is_empty() && !rest.starts_with([' ', '\t', '[']) {
        return None;
    }

    let rest = rest.trim_start();
    let rest = match rest.strip_prefix('[') {
        Some(after) => after.get(after.find(']')? + 1..)?.trim_start(),
        None => rest,
    };

    let end = rest
        .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .unwrap_or(rest.len());
    let name = &rest[..end];

    if name.is_empty() || !rest[end..].trim_start().is_empty() {
        return None;
    }

    Some(name)
}

fn implement_header_args(text: &str, offset: usize) -> Option<(String, usize)> {
    let offset = offset.min(text.len());
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut bracket = None;

    for index in (0..offset).rev() {
        match bytes[index] {
            b']' => depth += 1,
            b'[' => {
                if depth == 0 {
                    bracket = Some(index);
                    break;
                }

                depth -= 1;
            }
            b'{' | b'}' | b';' => return None,
            _ => {}
        }
    }

    let bracket = bracket?;
    let before = text.get(..bracket)?;
    let start = before.rfind("implement")?;

    if start > 0 {
        let previous = before.as_bytes()[start - 1];

        if previous.is_ascii_alphanumeric() || previous == b'_' {
            return None;
        }
    }

    let name = implement_header_name(&before[start..])?;

    let mut arg_index = 0usize;
    let mut depth = 0usize;

    for byte in bytes[bracket + 1..offset].iter().copied() {
        match byte {
            b'[' | b'(' => depth += 1,
            b']' | b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => arg_index += 1,
            _ => {}
        }
    }

    Some((name.to_string(), arg_index))
}

fn type_module_kind(kind: &ModuleKind) -> bool {
    matches!(
        kind,
        ModuleKind::Struct | ModuleKind::Enum | ModuleKind::Interface | ModuleKind::Alias
    )
}

fn push_type_item(
    name: &str,
    kind: CompletionItemKind,
    detail: Option<String>,
    prefix: &str,
    seen: &mut HashSet<String>,
    items: &mut Vec<CompletionItem>,
) {
    if !name.starts_with(prefix) || !seen.insert(name.to_string()) {
        return;
    }

    items.push(CompletionItem {
        label: name.to_string(),
        kind: Some(kind),
        detail,
        ..Default::default()
    });
}

fn type_completion_items(
    text: &str,
    analysis: &Analysis,
    prefix: &str,
    paths: ResolvePaths<'_>,
) -> Vec<CompletionItem> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();

    for builtin in CompilerType::all_names() {
        push_type_item(
            &builtin,
            CompletionItemKind::STRUCT,
            None,
            prefix,
            &mut seen,
            &mut items,
        );
    }

    for occurrence in &analysis.occurrences {
        if occurrence.role != Role::Type {
            continue;
        }

        let Some(name) = text.get(occurrence.offset..occurrence.offset + occurrence.len) else {
            continue;
        };

        let kind = occurrence
            .def
            .and_then(|def| analysis.type_kinds.get(&def))
            .map(|kind| match kind {
                TypeKind::Struct | TypeKind::Alias => CompletionItemKind::STRUCT,
                TypeKind::Enum => CompletionItemKind::ENUM,
                TypeKind::Interface => CompletionItemKind::INTERFACE,
            })
            .unwrap_or(CompletionItemKind::STRUCT);

        push_type_item(name, kind, None, prefix, &mut seen, &mut items);
    }

    for member in crate::modules::core_members() {
        if !type_module_kind(&member.kind) {
            continue;
        }

        push_type_item(
            &member.name,
            module_kind(&member.kind),
            Some(member.module.clone()),
            prefix,
            &mut seen,
            &mut items,
        );
    }

    if let (Some(dir), Some(root)) = (paths.dir, paths.project()) {
        for import in crate::modules::imports_for(text, paths.std) {
            for member in
                crate::modules::members_of_use(dir, root, paths.std, paths.packages(), &import)
            {
                if !type_module_kind(&member.kind) {
                    continue;
                }

                push_type_item(
                    &member.name,
                    module_kind(&member.kind),
                    Some(member.module.clone()),
                    prefix,
                    &mut seen,
                    &mut items,
                );
            }
        }
    }

    items
}

pub fn implement_header_complete(
    text: &str,
    analysis: &Analysis,
    offset: usize,
    paths: ResolvePaths<'_>,
) -> Vec<CompletionItem> {
    let Some((interface, arg_index)) = implement_header_args(text, offset) else {
        return Vec::new();
    };

    let arity = analysis
        .interface_arity
        .get(&interface)
        .copied()
        .unwrap_or(0);

    if arg_index >= arity {
        return Vec::new();
    }

    type_completion_items(text, analysis, ident_prefix(text, offset), paths)
}

pub fn implement_complete(text: &str, analysis: &Analysis, offset: usize) -> Vec<CompletionItem> {
    let frames = scan_frames(text, offset);
    let Some(index) = frames
        .iter()
        .rposition(|frame| matches!(frame.kind, FrameKind::Implement(_)))
    else {
        return Vec::new();
    };

    if index + 1 != frames.len() {
        return Vec::new();
    }

    let FrameKind::Implement(interface) = &frames[index].kind else {
        return Vec::new();
    };

    let Some(members) = analysis.interfaces.get(interface) else {
        return Vec::new();
    };

    let line_start = text[..offset.min(text.len())]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);

    if let Some(alias_prefix) = alias_decl_prefix(text, offset) {
        let bound = implemented_aliases(text, frames[index].body_start, line_start);
        let mut seen: HashSet<String> = HashSet::new();
        let mut items = Vec::new();

        for member in members {
            if member.kind != MemberKind::AssociatedType {
                continue;
            }

            if bound.iter().any(|name| name == &member.name)
                || !member.name.starts_with(alias_prefix)
                || !seen.insert(member.name.clone())
            {
                continue;
            }

            items.push(CompletionItem {
                label: member.name.clone(),
                kind: Some(CompletionItemKind::TYPE_PARAMETER),
                detail: Some(format!("alias {}", member.name)),
                insert_text: Some(format!("{} = $0;", member.name)),
                insert_text_format: Some(InsertTextFormat::SNIPPET),
                ..Default::default()
            });
        }

        return items;
    }

    let implemented = implemented_methods(text, frames[index].body_start);
    let typed = ident_prefix(text, offset);
    let has_fn = has_fn_keyword(text, offset, typed);
    let prefix = if typed == "fn" || typed == "pub" {
        ""
    } else {
        typed
    };

    let mut seen: HashSet<String> = HashSet::new();
    let mut items = Vec::new();

    for member in members {
        if member.kind != MemberKind::Method {
            continue;
        }

        if implemented.iter().any(|name| name == &member.name) {
            continue;
        }

        if !prefix.is_empty() && !member.name.starts_with(prefix) {
            continue;
        }

        if !seen.insert(member.name.clone()) {
            continue;
        }

        let signature = analysis.fn_source_sig.get(&member.def).cloned();
        let insert_text = signature.as_ref().map(|signature| {
            let signature = if has_fn {
                signature.strip_prefix("fn ").unwrap_or(signature)
            } else {
                signature
            };

            format!("{signature} {{\n\t$0\n}}")
        });
        let label_detail = signature
            .as_deref()
            .and_then(|signature| signature.strip_prefix("fn "))
            .and_then(|signature| signature.strip_prefix(member.name.as_str()))
            .map(|suffix| suffix.to_string());

        items.push(CompletionItem {
            label: member.name.clone(),
            kind: Some(CompletionItemKind::METHOD),
            detail: signature,
            label_details: label_detail.map(|detail| CompletionItemLabelDetails {
                detail: Some(detail),
                description: None,
            }),
            insert_text_format: insert_text.as_ref().map(|_| InsertTextFormat::SNIPPET),
            insert_text,
            ..Default::default()
        });
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
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'.')
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
    analysis
        .occurrences
        .iter()
        .find_map(|occurrence| {
            if occurrence.role == Role::Type
                && text.get(occurrence.offset..occurrence.offset + occurrence.len) == Some(name)
            {
                occurrence.def
            } else {
                None
            }
        })
        .or_else(|| {
            analysis
                .builtin_targets
                .iter()
                .find(|target| target.name == name)
                .map(|target| target.def)
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
        MemberKind::AssociatedType => CompletionItemKind::TYPE_PARAMETER,
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
        (Some(true), MemberKind::AssociatedType) => true,
        (Some(false), MemberKind::Method) => !member.is_static,
        (Some(false), MemberKind::Field) => true,
        (Some(false), MemberKind::Variant) => false,
        (Some(false), MemberKind::AssociatedType) => false,
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

fn member_label(name: &str, arity: Option<usize>) -> String {
    match arity {
        Some(0) => format!("{name}()"),
        Some(_) => format!("{name}(...)"),
        None => name.to_string(),
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

    let receiver_name = text.get(start..dot_offset).unwrap_or_default();

    let filter = forward_ident(text, dot_offset + 1).to_string();
    let mut static_context: Option<bool> = None;
    let mut type_name: Option<String> = None;
    let mut raw_type: Option<String> = None;

    if receiver_name == "Self"
        && start < dot_offset
        && let Some(def) = analysis.at(start).and_then(|receiver| receiver.def)
    {
        let def = analysis
            .interface_self_owners
            .get(&def)
            .copied()
            .unwrap_or(def);

        let mut items = Vec::new();
        let mut seen = std::collections::HashSet::new();

        push_owner_members(
            analysis,
            def,
            Some(true),
            dot_offset,
            &filter,
            &mut items,
            &mut seen,
        );

        return items;
    }

    if crate::builtin::is_scalar_type_name(receiver_name) {
        static_context = Some(true);
        type_name = Some(receiver_name.to_string());
    } else if start < dot_offset
        && let Some(receiver) = analysis.at(start)
        && receiver.role == Role::Type
    {
        let builtin = receiver.def.and_then(|def| {
            analysis
                .builtin_targets
                .iter()
                .find(|target| target.def == def)
        });

        if let Some(target) = builtin {
            static_context = Some(true);
            type_name = Some(target.name.clone());
        } else if let Some(name) = text.get(receiver.offset..receiver.offset + receiver.len) {
            static_context = Some(true);
            type_name = Some(name.to_string());
        }
    } else if let Some(rendered) = analysis.access_types.get(&(start, dot_offset - start)) {
        raw_type = Some(rendered.clone());

        if let Some(ty) = normalize_type_name(rendered) {
            static_context = Some(false);
            type_name = Some(ty);
        }
    } else if let Some(rendered) = expr_type_before(analysis, text, dot_offset) {
        raw_type = Some(rendered.clone());

        if let Some(ty) = normalize_type_name(&rendered) {
            static_context = Some(false);
            type_name = Some(ty);
        }
    } else if start < dot_offset
        && let Some(receiver) = analysis.at(start)
    {
        raw_type = receiver.ty.clone();

        if let Some(ty) = receiver.ty.as_deref().and_then(normalize_type_name) {
            static_context = Some(false);
            type_name = Some(ty);
        }
    } else if analysis
        .builtin_targets
        .iter()
        .any(|target| target.name == receiver_name)
    {
        static_context = Some(true);
        type_name = Some(receiver_name.to_string());
    }

    if type_name.is_none() && raw_type.is_none() && receiver_name.is_empty() {
        return Vec::new();
    }

    let lookup = type_name.as_deref().unwrap_or(receiver_name).to_string();

    let mut items = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    if push_variant_payload_members(
        analysis, text, &lookup, &filter, paths, &mut items, &mut seen,
    ) {
        return items;
    }

    let shape_matched = static_context != Some(true)
        && raw_type.as_deref().is_some_and(|ty| {
            push_builtin_target_members(analysis, text, ty, &filter, paths, &mut items, &mut seen)
        });

    if !shape_matched {
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
    }

    items
}

fn expr_type_before(analysis: &Analysis, text: &str, dot_offset: usize) -> Option<String> {
    let bytes = text.as_bytes();
    let mut best: Option<(&(usize, usize), &String)> = None;

    for (span, rendered) in &analysis.expr_types {
        let end = span.0.saturating_add(span.1);

        if end == 0 || end > dot_offset || end > bytes.len() {
            continue;
        }

        if !bytes[end..dot_offset]
            .iter()
            .all(|byte| *byte == b')' || byte.is_ascii_whitespace())
        {
            continue;
        }

        let better = match best {
            Some((current, _)) => match end.cmp(&(current.0 + current.1)) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Equal => span.0 < current.0,
                std::cmp::Ordering::Less => false,
            },
            None => true,
        };

        if better {
            best = Some((span, rendered));
        }
    }

    best.map(|(_, rendered)| rendered.clone())
}

fn push_variant_payload_members(
    analysis: &Analysis,
    text: &str,
    lookup: &str,
    filter: &str,
    paths: ResolvePaths<'_>,
    items: &mut Vec<CompletionItem>,
    seen: &mut std::collections::HashSet<String>,
) -> bool {
    let Some((_, variant_name)) = lookup.rsplit_once('.') else {
        return false;
    };

    let mut handled = false;

    if let Some(def) = find_variant_def(analysis, text, variant_name) {
        handled = true;

        if let Some(fields) = analysis.variant_fields.get(&def) {
            let mut names: Vec<&str> = fields
                .iter()
                .map(String::as_str)
                .filter(|name| name.starts_with(filter))
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
    }

    if let (Some(dir), Some(root)) = (paths.dir, paths.project()) {
        let mut payload = crate::modules::variant_payload_fields(
            dir,
            root,
            paths.std,
            paths.packages(),
            &crate::modules::imports_for(text, paths.std),
            variant_name,
        );
        payload.sort_by(|a, b| a.name.cmp(&b.name));

        for member in payload {
            if !member.name.starts_with(filter) {
                continue;
            }

            handled = true;

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

    handled
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
    if let Some(def) = find_type_def(analysis, text, lookup) {
        push_owner_members(analysis, def, static_context, cursor, filter, items, seen);
    }

    if let (Some(dir), Some(root)) = (paths.dir, paths.project()) {
        let mut imported = crate::modules::type_members(
            dir,
            root,
            paths.std,
            paths.packages(),
            &crate::modules::imports_for(text, paths.std),
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
                    label: member_label(&member.name, member.arity),
                    kind: Some(module_member_completion_kind(&member)),
                    detail: Some(member.module.clone()),
                    filter_text: Some(member.name.clone()),
                    insert_text: Some(member.name.clone()),
                    sort_text: Some(format!("{rank}_{}", member.name)),
                    ..Default::default()
                });
            }
        }
    }
}

fn push_owner_members(
    analysis: &Analysis,
    def: zeen_resolve::DefId,
    static_context: Option<bool>,
    cursor: usize,
    filter: &str,
    items: &mut Vec<CompletionItem>,
    seen: &mut std::collections::HashSet<String>,
) {
    let mut owners = vec![def];

    if let Some(bounds) = analysis.generic_bounds.get(&def) {
        owners.extend(bounds.iter().copied());
    }

    let allow_private = owners
        .iter()
        .any(|owner| scope_covers(analysis, *owner, cursor));
    let mut local: Vec<&Member> = Vec::new();

    for owner in &owners {
        if let Some(members) = analysis.members.get(owner) {
            local.extend(members.iter().filter(|member| {
                (member.is_pub || member.is_interface || allow_private)
                    && keep_local_member(member, static_context)
                    && member.name.starts_with(filter)
            }));
        }
    }

    local.sort_by(|a, b| a.name.cmp(&b.name));

    for member in local {
        push_member_item(member, static_context, items, seen);
    }

    for (iface_def, object_def) in &analysis.implements {
        if *object_def != def {
            continue;
        }
        let Some(iface_name) = analysis.interface_def_name.get(iface_def) else {
            continue;
        };
        let Some(iface_methods) = analysis.interfaces.get(iface_name) else {
            continue;
        };
        let same_file = analysis
            .def_spans
            .get(iface_def)
            .is_some_and(|(_, _, file)| file.is_none());
        for member in iface_methods {
            if seen.contains(&member.name) {
                continue;
            }
            if !member.name.starts_with(filter) {
                continue;
            }
            if !member.is_pub && !allow_private && !same_file {
                continue;
            }
            if !keep_local_member(member, static_context) {
                continue;
            }
            push_member_item(member, static_context, items, seen);
        }
    }
}

fn push_member_item(
    member: &Member,
    static_context: Option<bool>,
    items: &mut Vec<CompletionItem>,
    seen: &mut std::collections::HashSet<String>,
) {
    if !seen.insert(member.name.clone()) {
        return;
    }

    let rank = member_rank(
        member.kind == MemberKind::Variant
            || member.kind == MemberKind::AssociatedType
            || (member.kind == MemberKind::Method && member.is_static),
        static_context,
    );

    items.push(CompletionItem {
        label: member_label(&member.name, member.arity),
        kind: Some(member_completion_kind(member)),
        detail: member.signature.clone(),
        filter_text: Some(member.name.clone()),
        insert_text: Some(member.name.clone()),
        sort_text: Some(format!("{rank}_{}", member.name)),
        ..Default::default()
    });
}

fn push_builtin_target_members(
    analysis: &Analysis,
    text: &str,
    ty: &str,
    filter: &str,
    paths: ResolvePaths<'_>,
    items: &mut Vec<CompletionItem>,
    seen: &mut std::collections::HashSet<String>,
) -> bool {
    let ty = crate::builtin::strip_pointer_wrappers(ty);
    let mut matched: Vec<(&BuiltinTarget, bool)> = Vec::new();

    for target in &analysis.builtin_targets {
        if matches!(target.shape, BuiltinShape::Scalar(_)) {
            continue;
        }

        if crate::builtin::shape_matches_type(&target.shape, ty) {
            matched.push((target, target.shape.has_params()));
        }
    }

    if matched.is_empty() {
        return false;
    }

    matched.sort_by_key(|(target, has_params)| (*has_params, target.def.0));

    for (target, _) in &matched {
        let Some(members) = analysis.members.get(&target.def) else {
            continue;
        };

        for member in members {
            if !keep_local_member(member, Some(false)) || !member.name.starts_with(filter) {
                continue;
            }

            if !seen.insert(member.name.clone()) {
                continue;
            }

            let rank = member_rank(false, Some(false));

            items.push(CompletionItem {
                label: member_label(&member.name, member.arity),
                kind: Some(member_completion_kind(member)),
                detail: member.signature.clone(),
                filter_text: Some(member.name.clone()),
                insert_text: Some(member.name.clone()),
                sort_text: Some(format!("{rank}_{}", member.name)),
                ..Default::default()
            });
        }
    }

    push_imported_builtin_members(text, ty, filter, paths, items, seen);

    true
}

fn push_imported_builtin_members(
    text: &str,
    ty: &str,
    filter: &str,
    paths: ResolvePaths<'_>,
    items: &mut Vec<CompletionItem>,
    seen: &mut std::collections::HashSet<String>,
) {
    let (Some(dir), Some(root)) = (paths.dir, paths.project()) else {
        return;
    };

    let mut imported = crate::modules::builtin_shape_members(
        dir,
        root,
        paths.std,
        paths.packages(),
        &crate::modules::imports_for(text, paths.std),
        ty,
    );
    imported.sort_by(|a, b| a.name.cmp(&b.name));

    for member in imported {
        if !member.name.starts_with(filter) || !keep_module_member(&member, Some(false)) {
            continue;
        }

        if seen.insert(member.name.clone()) {
            let rank = member_rank(false, Some(false));

            items.push(CompletionItem {
                label: member_label(&member.name, member.arity),
                kind: Some(module_member_completion_kind(&member)),
                detail: Some(member.module.clone()),
                filter_text: Some(member.name.clone()),
                insert_text: Some(member.name.clone()),
                sort_text: Some(format!("{rank}_{}", member.name)),
                ..Default::default()
            });
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
    let Some(rendered) = analysis.call_types.get(&trigger.callee) else {
        return Vec::new();
    };

    let filter = forward_ident(text, trigger.dot + 1).to_string();
    let mut items = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let shape_matched = push_builtin_target_members(
        analysis, text, rendered, &filter, paths, &mut items, &mut seen,
    );

    if !shape_matched {
        let Some(ty) = normalize_type_name(rendered) else {
            return items;
        };

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
    }

    items
}

fn struct_init_open(text: &str, offset: usize) -> Option<usize> {
    let offset = offset.min(text.len());
    let bytes = text.as_bytes();
    let mut depth: usize = 0;
    let mut index = offset;

    loop {
        if index == 0 {
            return None;
        }

        index -= 1;

        match bytes[index] {
            b'}' => depth += 1,
            b'{' => {
                if depth == 0 {
                    return Some(index);
                }

                depth -= 1;
            }
            _ => {}
        }
    }
}

fn struct_init_type_name(text: &str, open: usize) -> Option<String> {
    let bytes = text.as_bytes();
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
        .map(str::to_string)
}

pub fn struct_init_context(text: &str, offset: usize) -> Option<(String, usize)> {
    let open = struct_init_open(text, offset)?;
    let name = struct_init_type_name(text, open)?;

    Some((name, open))
}

fn short_init_open(text: &str, open: usize) -> bool {
    open > 0 && text.as_bytes()[open - 1] == b'.'
}

fn short_init_type_name(text: &str, analysis: &Analysis, open: usize) -> Option<String> {
    if !short_init_open(text, open) {
        return None;
    }

    let dot = open - 1;

    let from_expr = analysis
        .expr_types
        .iter()
        .find_map(|((start, _), rendered)| {
            if *start == dot {
                normalize_type_name(rendered)
            } else {
                None
            }
        });

    from_expr.or_else(|| call_argument_type(analysis, open))
}

fn call_argument_type(analysis: &Analysis, open: usize) -> Option<String> {
    for call in &analysis.calls {
        for (index, (start, len)) in call.args.iter().enumerate() {
            if open < *start || open > start + len {
                continue;
            }

            if let Some(Some(rendered)) = call.param_types.get(index) {
                return normalize_type_name(rendered);
            }
        }
    }

    None
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
    let Some(open) = struct_init_open(text, offset) else {
        return Vec::new();
    };

    let Some(type_name) =
        struct_init_type_name(text, open).or_else(|| short_init_type_name(text, analysis, open))
    else {
        return Vec::new();
    };

    let filter = ident_prefix(text, offset).to_string();
    let used = struct_init_used_fields(text, open);
    let mut items = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    if let Some(def) = find_type_def(analysis, text, &type_name)
        && let Some(members) = analysis.members.get(&def)
    {
        let mut local: Vec<&Member> = members
            .iter()
            .filter(|member| {
                member.kind == MemberKind::Field
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
        let imports = crate::modules::imports_for(text, paths.std);
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

pub fn repair_for_plain(text: &str, offset: usize, semi: bool) -> Option<String> {
    let offset = offset.min(text.len());
    let bytes = text.as_bytes();
    let mut stack: Vec<u8> = Vec::new();
    let mut cursor = 0;

    while cursor < bytes.len() {
        match bytes[cursor] {
            b'"' => cursor = skip_string(bytes, cursor),
            b'/' if bytes.get(cursor + 1) == Some(&b'/') => cursor = skip_line(bytes, cursor),
            b'/' if bytes.get(cursor + 1) == Some(&b'*') => {
                cursor = skip_block_comment(bytes, cursor)
            }
            b'(' => {
                stack.push(b')');
                cursor += 1;
            }
            b'[' => {
                stack.push(b']');
                cursor += 1;
            }
            b'{' => {
                stack.push(b'}');
                cursor += 1;
            }
            b')' | b']' | b'}' => {
                stack.pop();
                cursor += 1;
            }
            _ => cursor += 1,
        }
    }

    let before = text.get(..offset).unwrap_or("").trim_end();

    if stack.is_empty() {
        if !semi || before.is_empty() || before.ends_with([';', '{', '}', '(', ',', '[']) {
            return None;
        }

        let mut repaired = String::with_capacity(text.len() + 1);
        repaired.push_str(&text[..offset]);
        repaired.push(';');
        repaired.push_str(&text[offset..]);

        return Some(repaired);
    }

    let mut repaired = String::with_capacity(text.len() + stack.len() + 1);
    repaired.push_str(&text[..offset]);

    for (index, close) in stack.iter().rev().enumerate() {
        if semi
            && index == 0
            && *close == b'}'
            && !before.is_empty()
            && !before.ends_with([';', '{', '}'])
        {
            repaired.push(';');
        }

        repaired.push(*close as char);

        if semi && index == 0 && matches!(*close, b')' | b']') {
            repaired.push(';');
        }
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
    let Some(open) = struct_init_open(text, offset) else {
        return false;
    };

    if struct_init_type_name(text, open).is_none() && !short_init_open(text, open) {
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
    fn std_modules_skip_project_src_segment() {
        let project = std::env::temp_dir().join(format!("zeen-lsp-std-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        std::fs::create_dir_all(project.join("src").join("c")).unwrap();

        std::fs::write(project.join("src").join("alloc.zn"), "").unwrap();
        std::fs::write(project.join("src").join("c").join("types.zn"), "").unwrap();

        let src = crate::diagnostics::std_src(project.clone()).unwrap();
        let modules = std_modules(&src);

        assert!(modules.contains(&"std.alloc".to_string()));
        assert!(modules.contains(&"std.c.types".to_string()));
        assert!(!modules.iter().any(|module| module.contains("src")));

        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn at_trigger_suggests_macros_and_directives() {
        let analysis = Analysis::default();

        let text = "@pr";
        let items = complete(text, &analysis, text.len(), ResolvePaths::default());
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();

        assert!(labels.contains(&"@print"));
        assert!(labels.contains(&"@println"));
        assert!(labels.iter().all(|label| label.starts_with('@')));

        let text = "@o";
        let items = complete(text, &analysis, text.len(), ResolvePaths::default());
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();

        assert!(labels.contains(&"@os"));
    }

    #[test]
    fn bracket_suggests_directive_values() {
        let analysis = Analysis::default();

        let text = "@os[lin";
        let items = complete(text, &analysis, text.len(), ResolvePaths::default());
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();

        assert_eq!(labels, vec!["linux"]);

        let text = "@var[";
        let items = complete(text, &analysis, text.len(), ResolvePaths::default());
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();

        assert!(labels.contains(&"os"));
        assert!(labels.contains(&"debug"));

        let text = "@foo[";
        let items = complete(text, &analysis, text.len(), ResolvePaths::default());

        assert!(items.is_empty());
    }

    #[test]
    fn method_def_span_covers_decl_name() {
        let analysis = sample_analysis(CHAIN_SAMPLE);
        let callee = CHAIN_SAMPLE.find("o.inner.get").unwrap() + "o.inner.".len();
        let fn_def = crate::hover::method_at(&analysis, (callee, "get".len())).unwrap();

        let (offset, len, file) = analysis.def_spans.get(&fn_def).cloned().unwrap();
        assert!(file.is_none());

        let name = CHAIN_SAMPLE.find("fn get(*self)").unwrap() + "fn ".len();
        assert!(offset <= name && name < offset + len);
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
        let errors = crate::diagnostics::only_errors(&output.diagnostics);
        assert!(errors.is_empty(), "sample must check cleanly: {:?}", errors);
        output.analysis
    }

    fn plain_analysis(text: &str, offset: usize) -> crate::analysis::Analysis {
        let uri: tower_lsp_server::ls_types::Uri = "file:///tmp/zeen-lsp-completion-plain-probe.zn"
            .parse()
            .unwrap();
        let first = crate::diagnostics::check_completion(&uri, text, &[]);

        if !first.analysis.occurrences.is_empty() {
            return first.analysis;
        }

        for semi in [false, true] {
            if let Some(candidate) = repair_for_plain(text, offset, semi) {
                let output = crate::diagnostics::check_completion(&uri, &candidate, &[]);

                if !output.analysis.occurrences.is_empty() {
                    return output.analysis;
                }
            }
        }

        crate::analysis::build_text_fallback(text, offset)
    }

    const SIG_SAMPLE: &str = "struct P {\n    pub v: i32,\n\n    pub fn at(*self, x: i32, y: i32) i32 {\n        return x;\n    }\n}\n\nfn main() {\n    let p = P { .v = 1 };\n    let q = p.at(1, 2);\n    @println(\"{}\", q);\n}\n";

    #[test]
    fn signature_skips_self_and_tracks_arg() {
        let analysis = sample_analysis(SIG_SAMPLE);

        let first = SIG_SAMPLE.find("p.at(1, 2)").unwrap() + "p.at(".len();
        let data = analysis.signature_at(first).unwrap();
        assert_eq!(data.label, "at(x: i32, y: i32) i32");
        assert_eq!(
            data.params,
            vec!["x: i32".to_string(), "y: i32".to_string()]
        );
        assert_eq!(data.active, Some(0));

        let second = SIG_SAMPLE.find(", 2)").unwrap() + ", ".len();
        let data = analysis.signature_at(second).unwrap();
        assert_eq!(data.active, Some(1));
    }

    #[test]
    fn struct_init_reports_field_hints() {
        let analysis = sample_analysis(SIG_SAMPLE);

        assert_eq!(analysis.field_hints.len(), 1);
        assert_eq!(analysis.field_hints[0].ty, "i32");
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

        assert!(names.contains(&"new(...)"));

        for hidden in ["len", "x", "y"] {
            assert!(!names.contains(&hidden), "static must hide {hidden}");
        }

        let new_item = items.iter().find(|item| item.label == "new(...)").unwrap();
        assert_eq!(new_item.kind, Some(CompletionItemKind::FUNCTION));
    }

    #[test]
    fn value_dot_shows_methods_and_fields() {
        let analysis = sample_analysis(SAMPLE);
        let dot = SAMPLE.find("p.len").unwrap() + "p".len();
        let items = dot_complete(SAMPLE, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["len()"]);

        let len_item = items.iter().find(|item| item.label == "len()").unwrap();
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
        let items = dot_complete(
            &broken,
            &fresh.analysis,
            offset - 1,
            ResolvePaths::default(),
        );
        let names = labels(&items);

        assert!(names.contains(&"len()"));
        assert!(names.contains(&"x"));
        assert!(!names.contains(&"new(...)"));

        let len_item = items.iter().find(|item| item.label == "len()").unwrap();
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

    const SWITCH_SAMPLE: &str = "enum Msg {\n    quit,\n    step: {\n        x: i32,\n        y: i32,\n    },\n    data: i32,\n}\n\nfn handle(m: Msg) i32 {\n    switch (m) {\n        .quit => 0,\n        .step (pos) => pos.x,\n        .data (d) => d,\n    }\n}\n\nfn main() {\n    @println(\"{}\", handle(Msg.quit));\n}\n";

    #[test]
    fn field_chain_resolves_middle_type() {
        let analysis = sample_analysis(CHAIN_SAMPLE);

        let dot = CHAIN_SAMPLE.find("o.inner.get").unwrap() + "o.inner".len();
        let items = dot_complete(CHAIN_SAMPLE, &analysis, dot, ResolvePaths::default());
        assert_eq!(labels(&items), vec!["get()"]);

        let dot = CHAIN_SAMPLE.find("o.inner.v").unwrap() + "o.inner".len();
        let items = dot_complete(CHAIN_SAMPLE, &analysis, dot, ResolvePaths::default());
        assert_eq!(labels(&items), vec!["v"]);
    }
    #[test]
    fn switch_payload_field_completes() {
        let analysis = sample_analysis(SWITCH_SAMPLE);
        let dot = SWITCH_SAMPLE.find("pos.x").unwrap() + "pos".len();
        let items = dot_complete(SWITCH_SAMPLE, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["x"]);
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

        assert!(names.contains(&"get()"));
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
        assert_eq!(names, vec!["make(...)"]);
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
        let errors = crate::diagnostics::only_errors(&output.diagnostics);
        assert!(
            errors.is_empty(),
            "package sample must check cleanly: {:?}",
            errors
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
        assert!(names.contains(&"make()"));
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
        let errors = crate::diagnostics::only_errors(&output.diagnostics);
        assert!(
            errors.is_empty(),
            "self import must check cleanly: {:?}",
            errors
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

    const IMPLEMENT_BLOCK: &str = "interface Describe {\n    fn kind(*const self) i32;\n    fn label(*const self) i32;\n}\n\nstruct Point {\n    pub x: i32,\n}\n\nimplement Describe : Point {\n}\n";

    fn loose_analysis(text: &str) -> crate::analysis::Analysis {
        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-implement-probe.zn".parse().unwrap();
        crate::diagnostics::check(&uri, text, &[]).analysis
    }

    #[test]
    fn implement_block_suggests_missing_methods() {
        let analysis = loose_analysis(IMPLEMENT_BLOCK);
        let anchor = "implement Describe : Point {";
        let offset = IMPLEMENT_BLOCK.find(anchor).unwrap() + anchor.len();
        let items = implement_complete(IMPLEMENT_BLOCK, &analysis, offset);
        let mut names = labels(&items);
        names.sort();

        assert_eq!(names, vec!["kind", "label"]);

        let kind = items.iter().find(|item| item.label == "kind").unwrap();
        assert_eq!(
            kind.insert_text.as_deref(),
            Some("fn kind(*const self) i32 {\n\t$0\n}")
        );
    }

    #[test]
    fn implement_block_hides_implemented_methods() {
        let text = "interface Describe {\n    fn kind(*const self) i32;\n    fn label(*const self) i32;\n}\n\nstruct Point {\n    pub x: i32,\n}\n\nimplement Describe : Point {\n    fn kind(*const self) i32 {\n        return self.x;\n    }\n\n}\n";
        let analysis = loose_analysis(text);
        let offset = text.rfind('}').unwrap() - 1;
        let items = implement_complete(text, &analysis, offset);
        let names = labels(&items);

        assert_eq!(names, vec!["label"]);

        let inside = text.find("self.x").unwrap();
        assert!(implement_complete(text, &analysis, inside).is_empty());
    }

    #[test]
    fn implement_block_offers_core_interface_methods() {
        let text = "struct P {\n    pub x: i32,\n}\n\nimplement Display : P {\n}\n";
        let analysis = loose_analysis(text);
        let anchor = "implement Display : P {";
        let offset = text.find(anchor).unwrap() + anchor.len();
        let items = implement_complete(text, &analysis, offset);
        let names = labels(&items);

        assert!(names.contains(&"display"));
    }

    #[test]
    fn implement_block_fires_while_typing_fn_name() {
        let valid = "struct P {\n    pub x: i32,\n}\n\nimplement Display : P {\n}\n";
        let analysis = loose_analysis(valid);
        let typing = "struct P {\n    pub x: i32,\n}\n\nimplement Display : P {\n    fn di";
        let items = implement_complete(typing, &analysis, typing.len());
        let names = labels(&items);

        assert!(names.contains(&"display"));
    }

    #[test]
    fn implement_block_omits_fn_when_already_typed() {
        let valid = "struct P {\n    pub x: i32,\n}\n\nimplement Drop : P {\n}\n";
        let analysis = loose_analysis(valid);
        let typing = "struct P {\n    pub x: i32,\n}\n\nimplement Drop : P {\n    fn dr";
        let items = implement_complete(typing, &analysis, typing.len());
        let insert = items
            .iter()
            .find(|item| item.label == "drop")
            .and_then(|item| item.insert_text.clone())
            .expect("drop must be suggested");

        assert!(insert.starts_with("drop("), "got {insert:?}");
    }

    #[test]
    fn implement_block_keeps_fn_on_fresh_line() {
        let valid = "struct P {\n    pub x: i32,\n}\n\nimplement Drop : P {\n}\n";
        let analysis = loose_analysis(valid);
        let typing = "struct P {\n    pub x: i32,\n}\n\nimplement Drop : P {\n    ";
        let items = implement_complete(typing, &analysis, typing.len());
        let insert = items
            .iter()
            .find(|item| item.label == "drop")
            .and_then(|item| item.insert_text.clone())
            .expect("drop must be suggested");

        assert!(insert.starts_with("fn drop("), "got {insert:?}");
    }

    const ASSOCIATED_IMPLEMENT: &str = "interface Labeled {\n    alias Tag = i32;\n    alias Name = i32;\n\n    pub fn label(*self) Tag;\n}\n\nstruct Point {\n    value: i32,\n}\n\nimplement Labeled : Point {\n    fn label(*self) Tag {\n        return self.value;\n    }\n}\n";

    #[test]
    fn implement_block_suggests_associated_types() {
        let analysis = loose_analysis(ASSOCIATED_IMPLEMENT);
        let typing = ASSOCIATED_IMPLEMENT.replacen("    fn label", "    alias \n    fn label", 1);
        let offset = typing.rfind("    alias ").unwrap() + "    alias ".len();
        let items = implement_complete(&typing, &analysis, offset);
        let mut names = labels(&items);
        names.sort();

        assert_eq!(names, vec!["Name", "Tag"]);

        let tag = items.iter().find(|item| item.label == "Tag").unwrap();
        assert_eq!(tag.insert_text.as_deref(), Some("Tag = $0;"));
        assert_eq!(tag.kind, Some(CompletionItemKind::TYPE_PARAMETER));
    }

    #[test]
    fn implement_block_filters_associated_types_by_prefix() {
        let analysis = loose_analysis(ASSOCIATED_IMPLEMENT);
        let typing = ASSOCIATED_IMPLEMENT.replacen("    fn label", "    alias T\n    fn label", 1);
        let offset = typing.rfind("    alias T").unwrap() + "    alias T".len();
        let items = implement_complete(&typing, &analysis, offset);
        let names = labels(&items);

        assert_eq!(names, vec!["Tag"]);
    }

    #[test]
    fn implement_block_keeps_alias_keyword_completion() {
        let analysis = loose_analysis(ASSOCIATED_IMPLEMENT);
        let typing = ASSOCIATED_IMPLEMENT.replacen("    fn label", "    alias\n    fn label", 1);
        let offset = typing.rfind("    alias").unwrap() + "    alias".len();
        let items = implement_complete(&typing, &analysis, offset);

        assert!(items.is_empty(), "{:?}", labels(&items));
    }

    #[test]
    fn implement_block_hides_bound_associated_types() {
        let analysis = loose_analysis(ASSOCIATED_IMPLEMENT);
        let typing = ASSOCIATED_IMPLEMENT.replacen(
            "    fn label",
            "    alias Tag = i32;\n    alias \n    fn label",
            1,
        );
        let offset = typing.rfind("    alias Tag = i32;\n    alias ").unwrap()
            + "    alias Tag = i32;\n    alias ".len();
        let items = implement_complete(&typing, &analysis, offset);
        let names = labels(&items);

        assert_eq!(names, vec!["Name"]);
    }

    const CONVERT_INTERFACE: &str = "interface[T] Convert {\n    fn convert(value: T) Self;\n}\n\nstruct Meters {\n    value: f64,\n}\n\nimplement Convert[i32] : Meters {\n    fn convert(value: i32) Self {\n        return Meters { .value = @as(f64, value) };\n    }\n}\n";

    #[test]
    fn implement_header_suggests_interface_argument_types() {
        let analysis = loose_analysis(CONVERT_INTERFACE);
        let typing = &CONVERT_INTERFACE[..CONVERT_INTERFACE.find("i32] : Meters").unwrap()];
        let items =
            implement_header_complete(typing, &analysis, typing.len(), ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"i32"), "{names:?}");
        assert!(names.contains(&"Meters"), "{names:?}");
        assert!(!names.contains(&"fn"), "{names:?}");
        assert!(!names.contains(&"let"), "{names:?}");
    }

    #[test]
    fn implement_header_filters_interface_argument_types() {
        let analysis = loose_analysis(CONVERT_INTERFACE);
        let mut typing =
            CONVERT_INTERFACE[..CONVERT_INTERFACE.find("i32] : Meters").unwrap()].to_string();
        typing.push_str("Me");
        let items =
            implement_header_complete(&typing, &analysis, typing.len(), ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["Meters"]);
    }

    #[test]
    fn implement_header_stops_after_interface_arity() {
        let analysis = loose_analysis(CONVERT_INTERFACE);
        let mut typing =
            CONVERT_INTERFACE[..CONVERT_INTERFACE.find("i32] : Meters").unwrap()].to_string();
        typing.push_str("i32, ");
        let items =
            implement_header_complete(&typing, &analysis, typing.len(), ResolvePaths::default());

        assert!(items.is_empty(), "{:?}", labels(&items));
    }

    #[test]
    fn implement_header_ignores_generic_type_arguments() {
        let text = "struct List[T] {\n    value: T,\n}\n\nfn take(value: List[";
        let analysis = loose_analysis(text);
        let items = implement_header_complete(text, &analysis, text.len(), ResolvePaths::default());

        assert!(items.is_empty(), "{:?}", labels(&items));
    }

    #[test]
    fn general_completion_includes_core_members() {
        let text = "fn main() {\n    \n}\n";
        let analysis = sample_analysis(text);
        let offset = text.find("    \n").unwrap() + 4;
        let items = complete(text, &analysis, offset, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"Option"), "core Option must be offered");
        assert!(names.contains(&"Result"), "core Result must be offered");
        assert!(names.contains(&"Display"), "core Display must be offered");
    }

    const SCOPE_SAMPLE: &str = "fn alpha() i32 {\n    let alpha_local = 1;\n    let alpha_other = alpha_local + 1;\n    return alpha_other;\n}\n\nfn beta() i32 {\n    let beta_local = 2;\n    return beta_local;\n}\n\nfn main() {\n    @println(\"{}\", alpha() + beta());\n}\n";

    #[test]
    fn completion_filters_locals_by_scope() {
        let analysis = sample_analysis(SCOPE_SAMPLE);
        let offset = SCOPE_SAMPLE.find("let alpha_other = ").unwrap() + "let alpha_other = ".len();
        let items = complete(SCOPE_SAMPLE, &analysis, offset, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"alpha_local"));
        assert!(!names.contains(&"beta_local"));
        assert!(!names.contains(&"alpha_other"));
    }

    const RESOLVE_ERROR_SAMPLE: &str = "fn alpha() i32 {\n    let alpha_local = 1;\n    let broken = missing_name;\n    let alpha_other = 2;\n    alpha_local\n}\n\nfn main() {\n    @println(\"{}\", alpha());\n}\n";

    #[test]
    fn completion_keeps_locals_when_resolve_fails() {
        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-resolve-probe.zn"
                .parse()
                .unwrap();
        let output = crate::diagnostics::check_completion(&uri, RESOLVE_ERROR_SAMPLE, &[]);
        assert!(!output.diagnostics.is_empty());
        assert!(!output.analysis.occurrences.is_empty());

        let offset =
            RESOLVE_ERROR_SAMPLE.find("let alpha_other = ").unwrap() + "let alpha_other = ".len();
        let items = complete(
            RESOLVE_ERROR_SAMPLE,
            &output.analysis,
            offset,
            ResolvePaths::default(),
        );
        let names = labels(&items);

        assert!(names.contains(&"alpha_local"));
        assert!(!names.contains(&"beta_local"));
    }

    const PARSE_ERROR_SAMPLE: &str = "fn add(a: i32, b: i32) i32 {\n    return a + b;\n}\n\nfn alpha() i32 {\n    let alpha_local = 1;\n    let alpha_other = 2;\n    let sum = add(";

    #[test]
    fn completion_keeps_locals_when_parsing_fails() {
        let offset = PARSE_ERROR_SAMPLE.len();

        assert!(!crate::diagnostics::parses(PARSE_ERROR_SAMPLE));
        assert!(repair_for_plain(PARSE_ERROR_SAMPLE, offset, true).is_some());

        let analysis = plain_analysis(PARSE_ERROR_SAMPLE, offset);
        let items = complete(
            PARSE_ERROR_SAMPLE,
            &analysis,
            offset,
            ResolvePaths::default(),
        );
        let names = labels(&items);

        assert!(names.contains(&"alpha_local"));
        assert!(names.contains(&"alpha_other"));
    }

    const INTERFACE_SAMPLE: &str = "interface Describe {\n    pub fn label() []const char;\n    pub fn kind(*const self) []const char;\n}\n\nstruct Point {\n    pub x: i32,\n}\n\nimplement Describe : Point {\n    fn label() []const char {\n        \"point\"\n    }\n\n    fn kind(*const self) []const char {\n        \"kind\"\n    }\n}\n\nfn use_point(p: *const Point) []const char {\n    return p.kind();\n}\n\nfn main() {\n    let p = Point { .x = 1 };\n    @println(\"{} {}\", p.kind(), Point.label());\n}\n";

    #[test]
    fn implement_methods_complete_through_interface() {
        let analysis = sample_analysis(INTERFACE_SAMPLE);

        let probe = INTERFACE_SAMPLE.replacen("return p.kind();", "return p.      ;", 1);
        let dot = probe.find("p.      ;").unwrap() + "p".len();
        let items = dot_complete(&probe, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"kind()"));
        assert!(names.contains(&"x"));
        assert!(!names.contains(&"label()"));

        let probe = INTERFACE_SAMPLE.replacen("Point.label()", "Point.        ()", 1);
        let dot = probe.find("Point.        ()").unwrap() + "Point".len();
        let items = dot_complete(&probe, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"label()"));
        assert!(!names.contains(&"kind()"));
        assert!(!names.contains(&"x"));
    }

    const INTERFACE_RECEIVER_SAMPLE: &str = "interface Describe {\n    pub fn kind(*const self) i32;\n}\n\nfn describe(d: Describe) i32 {\n    return 0;\n}\n\nfn main() {\n    @println(\"{}\", 1);\n}\n";

    #[test]
    fn interface_typed_receiver_completes_interface_methods() {
        let analysis = sample_analysis(INTERFACE_RECEIVER_SAMPLE);
        let probe = INTERFACE_RECEIVER_SAMPLE.replacen(
            "fn describe(d: Describe)",
            "fn describe(d.          Describe)",
            1,
        );
        let dot = probe.find("d.          Describe").unwrap() + "d".len();
        let items = dot_complete(&probe, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"kind()"));
    }

    const GENERIC_BOUND_SAMPLE: &str = "interface Describe {\n    pub fn kind(*const self) i32;\n}\n\nstruct Point {\n    pub x: i32,\n}\n\nimplement Describe : Point {\n    fn kind(*const self) i32 {\n        return self.x;\n    }\n}\n\nfn show[T: Describe](x: *const T) i32 {\n    return (*x).kind();\n}\n\nfn main() {\n    let p = Point { .x = 1 };\n    @println(\"{}\", show(&p));\n}\n";

    #[test]
    fn generic_bound_receiver_completes_bound_methods() {
        let analysis = sample_analysis(GENERIC_BOUND_SAMPLE);
        let probe = GENERIC_BOUND_SAMPLE.replacen("(*x).kind()", "(*x.          )", 1);
        let dot = probe.find("x.          )").unwrap() + "x".len();
        let items = dot_complete(&probe, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"kind()"));
    }

    const ASSOCIATED_TYPE_SAMPLE: &str = "interface Source {\n    alias Item;\n\n    pub fn read(*self) Item;\n}\n\nstruct Number {\n    value: i32,\n}\n\nimplement Source : Number {\n    alias Item = i32;\n\n    fn read(*self) Item {\n        return self.value;\n    }\n}\n\nfn read_item[S: Source](source: *S) S.Item {\n    return source.read();\n}\n\nfn take_number(value: Number.Item) i32 {\n    return value;\n}\n\nfn main() {\n    let number = Number { .value = 3 };\n    let value: i32 = read_item(&number);\n    @println(\"{}\", take_number(value));\n}\n";

    #[test]
    fn generic_projection_completes_associated_types() {
        let analysis = sample_analysis(ASSOCIATED_TYPE_SAMPLE);
        let probe = ASSOCIATED_TYPE_SAMPLE.replacen("S.Item {", "S.        {", 1);
        let dot = probe.find("S.        {").unwrap() + 1;
        let items = dot_complete(&probe, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"Item"), "{names:?}");
    }

    #[test]
    fn concrete_projection_completes_associated_types() {
        let analysis = sample_analysis(ASSOCIATED_TYPE_SAMPLE);
        let probe = ASSOCIATED_TYPE_SAMPLE.replacen("Number.Item)", "Number.        )", 1);
        let dot = probe.find("Number.        )").unwrap() + "Number".len();
        let items = dot_complete(&probe, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"Item"), "{names:?}");
    }

    #[test]
    fn self_projection_completes_associated_types() {
        let probe_sample =
            ASSOCIATED_TYPE_SAMPLE.replacen("fn read(*self) Item", "fn read(*self) Self.Item", 1);
        let analysis = sample_analysis(&probe_sample);
        let probe = probe_sample.replacen("Self.Item", "Self.        ", 1);
        let dot = probe.find("Self.        ").unwrap() + "Self".len();
        let items = dot_complete(&probe, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"Item"), "{names:?}");
    }

    #[test]
    fn imported_impl_methods_ignore_privacy() {
        let dir = import_project("implprivacy");
        std::fs::write(
            dir.join("lib.zn"),
            "pub interface Describe {\n    pub fn kind(*const self) i32;\n}\n\npub struct Boxed {\n    pub v: i32,\n}\n\nimplement Describe : Boxed {\n    fn kind(*const self) i32 {\n        return self.v;\n    }\n}\n",
        )
        .unwrap();

        let members =
            crate::modules::type_members(&dir, &dir, None, &[], &["lib".to_string()], "Boxed");
        let names: Vec<&str> = members.iter().map(|member| member.name.as_str()).collect();

        assert!(names.contains(&"kind"));
        assert!(names.contains(&"v"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    const INHERENT_SAMPLE: &str = "struct Point {\n    pub x: i32,\n    pub y: i32,\n}\n\nimplement Point {\n    pub fn kind(*const self) i32 {\n        return 1;\n    }\n\n    pub fn plus(*const self, dx: i32) i32 {\n        return self.x + dx;\n    }\n\n    pub fn origin() Point {\n        return Point { .x = 0, .y = 0 };\n    }\n}\n\nfn main() {\n    let a = Point { .x = 1, .y = 2 };\n    @println(\"{} {}\", a.kind(), Point.origin().x);\n}\n";

    #[test]
    fn inherent_implement_members_complete() {
        let analysis = sample_analysis(INHERENT_SAMPLE);

        let probe = INHERENT_SAMPLE.replacen("a.kind()", "a.        ()", 1);
        let dot = probe.find("a.        ()").unwrap() + 1;
        let items = dot_complete(&probe, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"kind()"));
        assert!(names.contains(&"plus(...)"));
        assert!(names.contains(&"x"));
        assert!(!names.contains(&"origin"));

        let probe = INHERENT_SAMPLE.replacen("Point.origin()", "Point.        ()", 1);
        let dot = probe.find("Point.        ()").unwrap() + "Point".len();
        let items = dot_complete(&probe, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"origin()"));
        assert!(!names.contains(&"kind()"));
        assert!(!names.contains(&"x"));
    }

    #[test]
    fn dot_completion_works_in_file_with_type_errors() {
        let text = "struct Point {\n    pub x: i32,\n}\n\nfn main() {\n    let a = Point { .x = 1 };\n    @println(\"{}\", a.);\n}\n";
        let offset = text.find("a.);").unwrap() + 2;
        let repaired = repair_for_completion(text, offset).unwrap();
        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-errors.zn".parse().unwrap();
        let output = crate::diagnostics::check_completion(&uri, &repaired, &[]);
        let items = dot_complete(text, &output.analysis, offset - 1, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"x"));
    }

    #[test]
    fn dot_completion_works_when_method_is_typed_as_field() {
        let text = "struct Point {\n    pub x: i32,\n}\n\nimplement Point {\n    pub fn kind(*const self) i32 {\n        return 1;\n    }\n}\n\nfn main() {\n    let a = Point { .x = 1 };\n    let k = a.kind;\n    @println(\"{}\", k);\n}\n";
        let uri: tower_lsp_server::ls_types::Uri = "file:///tmp/zeen-lsp-completion-typed-field.zn"
            .parse()
            .unwrap();
        let output = crate::diagnostics::check(&uri, text, &[]);
        let dot = text.find("a.kind").unwrap() + 1;
        let items = dot_complete(text, &output.analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"kind()"));
        assert!(!names.contains(&"x"));
    }

    const BUILTIN_SAMPLE: &str = "implement builtin i32 {\n    fn is_even(*const self) bool {\n        return (*self) % 2 == 0;\n    }\n\n    fn bump(*self) void {\n        *self += 1;\n    }\n\n    fn max_value() i32 {\n        return 2147483647;\n    }\n}\n\nfn main() {\n    let n: i32 = 4;\n    let p: *i32 = &n;\n    n.bump();\n    @println(\"{} {} {}\", n.is_even(), p.is_even(), i32.max_value());\n}\n";

    #[test]
    fn builtin_value_dot_completes_methods() {
        let analysis = sample_analysis(BUILTIN_SAMPLE);
        let dot = BUILTIN_SAMPLE.find("n.is_even").unwrap() + "n".len();
        let items = dot_complete(BUILTIN_SAMPLE, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["is_even()"]);

        let even = items.iter().find(|item| item.label == "is_even()").unwrap();
        assert_eq!(even.kind, Some(CompletionItemKind::METHOD));
    }

    #[test]
    fn primitive_module_methods_complete_without_import() {
        let std_root =
            std::env::temp_dir().join(format!("zeen-lsp-primitive-std-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&std_root);
        std::fs::create_dir_all(std_root.join("primitives")).unwrap();
        std::fs::write(
            std_root.join("primitives").join("i8.zn"),
            "implement builtin i8 {\n    pub fn abs(*const self) i8 {\n        return *self;\n    }\n}\n",
        )
        .unwrap();

        let text = "fn main() {\n    let n: i8 = 3;\n    let x = n;\n}\n";
        let analysis = sample_analysis(text);
        let probe = text.replacen("let x = n;", "let x = n.   ();", 1);
        let dot = probe.find("n.   ()").unwrap() + 1;

        let paths = ResolvePaths {
            dir: Some(std_root.as_path()),
            root: Some(std_root.as_path()),
            std: Some(std_root.as_path()),
            packages: None,
        };

        let items = dot_complete(&probe, &analysis, dot, paths);
        let names = labels(&items);

        assert_eq!(names, vec!["abs()"]);

        let _ = std::fs::remove_dir_all(&std_root);
    }

    #[test]
    fn builtin_pointer_receiver_dot_completes_methods() {
        let analysis = sample_analysis(BUILTIN_SAMPLE);
        let dot = BUILTIN_SAMPLE.find("p.is_even").unwrap() + "p".len();
        let items = dot_complete(BUILTIN_SAMPLE, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["is_even()"]);
    }

    const EXPR_RECEIVER_SAMPLE: &str = "struct Marker {\n    pub fn count(*const self) i32 {\n        return 0;\n    }\n}\n\nfn make() Marker {\n    return Marker {};\n}\n\nfn main() {\n    let a = (Marker {}).count();\n    let b = make().count();\n}\n";

    #[test]
    fn grouped_expression_dot_completes_methods() {
        let analysis = sample_analysis(EXPR_RECEIVER_SAMPLE);
        let dot = EXPR_RECEIVER_SAMPLE.find(").count").unwrap() + 1;
        let items = dot_complete(
            EXPR_RECEIVER_SAMPLE,
            &analysis,
            dot,
            ResolvePaths::default(),
        );

        assert_eq!(labels(&items), vec!["count()"]);
    }

    #[test]
    fn call_expression_dot_completes_methods() {
        let analysis = sample_analysis(EXPR_RECEIVER_SAMPLE);
        let dot = EXPR_RECEIVER_SAMPLE.find("make().count").unwrap() + "make()".len();
        let items = dot_complete(
            EXPR_RECEIVER_SAMPLE,
            &analysis,
            dot,
            ResolvePaths::default(),
        );

        assert_eq!(labels(&items), vec!["count()"]);
    }

    #[test]
    fn literal_receiver_dot_completes_methods() {
        let std_root =
            std::env::temp_dir().join(format!("zeen-lsp-literal-std-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&std_root);
        std::fs::create_dir_all(std_root.join("primitives")).unwrap();
        std::fs::write(
            std_root.join("primitives").join("i32.zn"),
            "implement builtin i32 {\n    pub fn abs(*const self) i32 {\n        return *self;\n    }\n}\n",
        )
        .unwrap();

        let paths = ResolvePaths {
            dir: Some(std_root.as_path()),
            root: Some(std_root.as_path()),
            std: Some(std_root.as_path()),
            packages: None,
        };

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-literal-probe.zn".parse().unwrap();

        for text in [
            "fn main() {\n    let a = 42.abs();\n}\n",
            "fn main() {\n    let a = (42).abs();\n}\n",
        ] {
            let analysis = crate::diagnostics::check(&uri, text, &[]).analysis;
            let dot = text.find("42").unwrap() + 2;
            let items = dot_complete(text, &analysis, dot, paths);

            assert_eq!(labels(&items), vec!["abs()"], "text: {text}");
        }

        let _ = std::fs::remove_dir_all(&std_root);
    }

    #[test]
    fn builtin_static_dot_completes_functions() {
        let analysis = sample_analysis(BUILTIN_SAMPLE);
        let dot = BUILTIN_SAMPLE.find("i32.max_value").unwrap() + "i32".len();
        let items = dot_complete(BUILTIN_SAMPLE, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["max_value()"]);

        let max = items
            .iter()
            .find(|item| item.label == "max_value()")
            .unwrap();
        assert_eq!(max.kind, Some(CompletionItemKind::FUNCTION));
    }

    #[test]
    fn builtin_static_dot_completes_from_stale_analysis() {
        const STALE: &str = "implement builtin i32 {\n    pub fn max_value() i32 {\n        return 2147483647;\n    }\n}\n\nfn main() {\n    let n: i32 = 4;\n    let x = n;\n}\n";
        const CURRENT: &str = "fn main() {\n    let x = i32.;\n}\n";

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-stale-static.zn"
                .parse()
                .unwrap();
        let output = crate::diagnostics::check(&uri, STALE, &[]);
        let dot = CURRENT.find("i32.").unwrap() + "i32".len();
        let items = dot_complete(CURRENT, &output.analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["max_value()"]);
    }

    const BUILTIN_SLICE_SAMPLE: &str = "implement[T] builtin []T {\n    fn count(*const self) usize {\n        return (*self).len;\n    }\n\n    fn first(*const self) Option[T] {\n        return Option.Some((*self)[0]);\n    }\n}\n\nfn main() {\n    let nums: [4]i32 = {1, 2, 3, 4};\n    let slice = nums[..];\n    @println(\"{} {}\", slice.count(), slice.first().unwrap());\n}\n";

    #[test]
    fn builtin_slice_receiver_dot_completes_shape_methods() {
        let analysis = sample_analysis(BUILTIN_SLICE_SAMPLE);
        let dot = BUILTIN_SLICE_SAMPLE.find("slice.count").unwrap() + "slice".len();
        let items = dot_complete(
            BUILTIN_SLICE_SAMPLE,
            &analysis,
            dot,
            ResolvePaths::default(),
        );
        let names = labels(&items);

        assert_eq!(names, vec!["count()"]);
    }

    #[test]
    fn imported_builtin_shape_receiver_dot_completes_methods() {
        let dir = import_project("builtin-shape");
        std::fs::write(
            dir.join("sub").join("lib.zn"),
            "implement[T] builtin []T {\n    pub fn first(*const self) T {\n        return (*self)[0];\n    }\n}\n",
        )
        .unwrap();

        let text = "use sub.lib;\n\nfn main() {\n    let nums: [3]i32 = {1, 2, 3};\n    let slice = nums[..];\n    @println(\"{}\", slice.first());\n}\n";
        let uri: tower_lsp_server::ls_types::Uri =
            format!("file://{}", dir.join("main.zn").display())
                .parse()
                .unwrap();
        let output = crate::diagnostics::check(&uri, text, &[]);
        let errors = crate::diagnostics::only_errors(&output.diagnostics);
        assert!(
            errors.is_empty(),
            "shape import sample must check cleanly: {:?}",
            errors
        );

        let probe = text.replacen("slice.first()", "slice.     ()", 1);
        let dot = probe.find("slice.     ()").unwrap() + "slice".len();
        let paths = ResolvePaths {
            dir: Some(dir.as_path()),
            root: Some(dir.as_path()),
            std: None,
            packages: None,
        };
        let items = dot_complete(&probe, &output.analysis, dot, paths);
        let names = labels(&items);

        assert!(names.contains(&"first()"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    const BUILTIN_ARRAY_SAMPLE: &str = "implement builtin [4]u8 {\n    fn total(*const self) u32 {\n        return @as(u32, (*self)[0]) + @as(u32, (*self)[1]);\n    }\n}\n\nfn main() {\n    let data: [4]u8 = {1, 2, 3, 4};\n    @println(\"{}\", data.total());\n}\n";

    #[test]
    fn builtin_array_receiver_dot_completes_shape_methods() {
        let analysis = sample_analysis(BUILTIN_ARRAY_SAMPLE);
        let dot = BUILTIN_ARRAY_SAMPLE.find("data.total").unwrap() + "data".len();
        let items = dot_complete(
            BUILTIN_ARRAY_SAMPLE,
            &analysis,
            dot,
            ResolvePaths::default(),
        );
        let names = labels(&items);

        assert_eq!(names, vec!["total()"]);
    }

    const BUILTIN_MANY_POINTER_SAMPLE: &str = "implement builtin [*]i32 {\n    fn head(*const self) i32 {\n        return (*self)[0];\n    }\n}\n\nfn main() {\n    let p: [*]i32 = @uninit();\n    @println(\"{}\", p.head());\n}\n";

    #[test]
    fn builtin_many_pointer_receiver_dot_completes_shape_methods() {
        let analysis = sample_analysis(BUILTIN_MANY_POINTER_SAMPLE);
        let dot = BUILTIN_MANY_POINTER_SAMPLE.find("p.head").unwrap() + "p".len();
        let items = dot_complete(
            BUILTIN_MANY_POINTER_SAMPLE,
            &analysis,
            dot,
            ResolvePaths::default(),
        );
        let names = labels(&items);

        assert_eq!(names, vec!["head()"]);
    }

    const BUILTIN_SELF_SAMPLE: &str = "implement builtin i32 {\n    fn zero() i32 {\n        return 0;\n    }\n\n    fn forward() i32 {\n        return Self.zero();\n    }\n}\n\nfn main() {\n    @println(\"{}\", i32.forward());\n}\n";

    #[test]
    fn builtin_self_dot_completes_static_functions() {
        let analysis = sample_analysis(BUILTIN_SELF_SAMPLE);
        let dot = BUILTIN_SELF_SAMPLE.find("Self.zero").unwrap() + "Self".len();
        let items = dot_complete(BUILTIN_SELF_SAMPLE, &analysis, dot, ResolvePaths::default());
        let names = labels(&items);

        assert_eq!(names, vec!["zero()"]);
    }

    const BUILTIN_INTERFACE_SAMPLE: &str = "interface Show {\n    fn show(*const self) []const char;\n\n    fn tag() u8;\n}\n\nimplement Show : builtin i32 {\n    fn show(*const self) []const char {\n        return \"i32\";\n    }\n\n    fn tag() u8 {\n        return 7;\n    }\n}\n\nfn main() {\n    let n: i32 = 5;\n    @println(\"{} {}\", n.show(), i32.tag());\n}\n";

    #[test]
    fn builtin_interface_receiver_dot_completes_methods() {
        let analysis = sample_analysis(BUILTIN_INTERFACE_SAMPLE);
        let dot = BUILTIN_INTERFACE_SAMPLE.find("n.show").unwrap() + "n".len();
        let items = dot_complete(
            BUILTIN_INTERFACE_SAMPLE,
            &analysis,
            dot,
            ResolvePaths::default(),
        );
        let names = labels(&items);

        assert_eq!(names, vec!["show()"]);
    }

    #[test]
    fn builtin_interface_static_dot_completes_methods() {
        let analysis = sample_analysis(BUILTIN_INTERFACE_SAMPLE);
        let dot = BUILTIN_INTERFACE_SAMPLE.find("i32.tag").unwrap() + "i32".len();
        let items = dot_complete(
            BUILTIN_INTERFACE_SAMPLE,
            &analysis,
            dot,
            ResolvePaths::default(),
        );
        let names = labels(&items);

        assert_eq!(names, vec!["tag()"]);
    }

    const BUILTIN_INTERFACE_SHAPE_SAMPLE: &str = "interface Countable {\n    fn count(*const self) usize;\n}\n\nimplement[T] Countable : builtin []T {\n    fn count(*const self) usize {\n        return (*self).len;\n    }\n}\n\nfn main() {\n    let nums: [4]i32 = {1, 2, 3, 4};\n    let slice = nums[..];\n    @println(\"{}\", slice.count());\n}\n";

    #[test]
    fn builtin_interface_shape_receiver_dot_completes_methods() {
        let analysis = sample_analysis(BUILTIN_INTERFACE_SHAPE_SAMPLE);
        let dot = BUILTIN_INTERFACE_SHAPE_SAMPLE.find("slice.count").unwrap() + "slice".len();
        let items = dot_complete(
            BUILTIN_INTERFACE_SHAPE_SAMPLE,
            &analysis,
            dot,
            ResolvePaths::default(),
        );
        let names = labels(&items);

        assert_eq!(names, vec!["count()"]);
    }

    #[test]
    fn imported_builtin_interface_shape_receiver_dot_completes_methods() {
        let dir = import_project("builtin-interface-shape");
        std::fs::write(
            dir.join("sub").join("lib.zn"),
            "pub interface Countable {\n    pub fn count(*const self) usize;\n}\n\nimplement[T] Countable : builtin []T {\n    pub fn count(*const self) usize {\n        return (*self).len;\n    }\n}\n",
        )
        .unwrap();

        let text = "use sub.lib;\n\nfn main() {\n    let nums: [4]i32 = {1, 2, 3, 4};\n    let slice = nums[..];\n    @println(\"{}\", slice.count());\n}\n";
        let uri: tower_lsp_server::ls_types::Uri =
            format!("file://{}", dir.join("main.zn").display())
                .parse()
                .unwrap();
        let output = crate::diagnostics::check(&uri, text, &[]);
        let errors = crate::diagnostics::only_errors(&output.diagnostics);
        assert!(
            errors.is_empty(),
            "interface shape import sample must check cleanly: {:?}",
            errors
        );

        let probe = text.replacen("slice.count()", "slice.     ()", 1);
        let dot = probe.find("slice.     ()").unwrap() + "slice".len();
        let paths = ResolvePaths {
            dir: Some(dir.as_path()),
            root: Some(dir.as_path()),
            std: None,
            packages: None,
        };
        let items = dot_complete(&probe, &output.analysis, dot, paths);
        let names = labels(&items);

        assert!(names.contains(&"count()"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn static_dot_in_statement_position_completes() {
        let text = "struct Allocator {\n}\n\nimplement Allocator {\n    pub fn foo() i32 {\n        return 1;\n    }\n}\n\nfn main() {\n  Allocator.\n}\n";
        let offset = text.find("Allocator.").unwrap() + "Allocator.".len();
        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-static-statement.zn".parse().unwrap();
        let repaired = repair_for_completion(text, offset).unwrap();
        let output = crate::diagnostics::check_completion(&uri, &repaired, &[]);
        let items = dot_complete(text, &output.analysis, offset - 1, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"foo()"));

        for hidden in ["bit_or", "add", "deref"] {
            assert!(!names.contains(&hidden), "static must hide {hidden}");
        }
    }

    #[test]
    fn short_init_completes_fields_in_call_argument() {
        let text = "struct AllocatorInstance {\n    pub alloc: i32,\n    pub free: i32,\n}\n\nstruct Allocator {\n    pub fn set_allocator(instance: AllocatorInstance) {\n        let _ = instance;\n    }\n}\n\nfn main() {\n    Allocator.set_allocator(.{\n        .\n    });\n}\n";
        let offset = text.find("        .\n    });").unwrap() + "        .".len();

        assert!(struct_init_field_position(text, offset));

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-short-init.zn".parse().unwrap();
        let repaired = repair_for_completion(text, offset).unwrap();
        let output = crate::diagnostics::check_completion(&uri, &repaired, &[]);
        let items = struct_init_complete(text, &output.analysis, offset, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"alloc"));
        assert!(names.contains(&"free"));
    }

    #[test]
    fn short_init_skips_used_fields() {
        let text = "struct Point {\n    pub x: i32,\n    pub y: i32,\n}\n\nfn take(p: Point) {\n    let _ = p;\n}\n\nfn main() {\n    take(.{ .x = 1, . });\n}\n";
        let offset = text.find(".{ .x = 1, .").unwrap() + ".{ .x = 1, .".len();

        assert!(struct_init_field_position(text, offset));

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-short-init-used.zn".parse().unwrap();
        let repaired = repair_for_completion(text, offset).unwrap();
        let output = crate::diagnostics::check_completion(&uri, &repaired, &[]);
        let items = struct_init_complete(text, &output.analysis, offset, ResolvePaths::default());
        let names = labels(&items);

        assert!(names.contains(&"y"));
        assert!(!names.contains(&"x"));
    }

    #[test]
    fn std_allocator_static_and_short_init_complete() {
        let Some(std_root) = crate::diagnostics::resolve_std_root() else {
            return;
        };

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-std-probe.zn".parse().unwrap();
        let paths = ResolvePaths {
            dir: Some(std::path::Path::new("/tmp")),
            root: Some(std::path::Path::new("/tmp")),
            std: Some(std_root.as_path()),
            packages: None,
        };

        let text = "use std.alloc;\n\nfn main() {\n  Allocator.\n}\n";
        let offset = text.find("Allocator.").unwrap() + "Allocator.".len();
        let repaired = repair_for_completion(text, offset).unwrap();
        let output = crate::diagnostics::check_completion(&uri, &repaired, &[]);
        let items = dot_complete(text, &output.analysis, offset - 1, paths);
        let names = labels(&items);

        assert!(names.contains(&"alloc(...)"));
        assert!(names.contains(&"set_allocator(...)"));
        assert!(!names.contains(&"free"));

        let text = "use std.alloc;\n\nfn main() {\n  Allocator.set_allocator(.{\n    .\n  });\n}\n";
        let offset = text.find("    .\n  });").unwrap() + "    .".len();

        assert!(struct_init_field_position(text, offset));

        let repaired = repair_for_completion(text, offset).unwrap();
        let output = crate::diagnostics::check_completion(&uri, &repaired, &[]);
        let items = struct_init_complete(text, &output.analysis, offset, paths);
        let names = labels(&items);

        assert!(names.contains(&"alloc"));
        assert!(names.contains(&"free"));
        assert!(names.contains(&"realloc"));
    }

    #[test]
    fn enum_variants_complete_after_type_name() {
        let text =
            "enum Msg {\n    quit,\n    step: i32,\n}\n\nfn main() {\n    let m = Msg.;\n}\n";
        let dot = text.find("Msg.").unwrap() + "Msg".len();
        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-enum.zn".parse().unwrap();

        let repaired = repair_for_completion(text, dot + 1).unwrap();
        let output = crate::diagnostics::check_completion(&uri, &repaired, &[]);
        assert!(!output.analysis.occurrences.is_empty());

        let items = dot_complete(text, &output.analysis, dot, ResolvePaths::default());

        assert_eq!(labels(&items), vec!["quit", "step"]);
    }

    #[test]
    fn core_enum_variants_complete_after_type_name() {
        let uri: tower_lsp_server::ls_types::Uri = "file:///tmp/zeen-lsp-completion-core-enum.zn"
            .parse()
            .unwrap();

        let option = "fn main() {\n    let o = Option.;\n}\n";
        let dot = option.find("Option.").unwrap() + "Option".len();

        let repaired = repair_for_completion(option, dot + 1).unwrap();
        let output = crate::diagnostics::check_completion(&uri, &repaired, &[]);
        assert!(!output.analysis.occurrences.is_empty());

        let items = dot_complete(option, &output.analysis, dot, ResolvePaths::default());

        assert_eq!(labels(&items), vec!["None", "Some"]);

        let result = "fn main() {\n    let r = Result.;\n}\n";
        let dot = result.find("Result.").unwrap() + "Result".len();

        let repaired = repair_for_completion(result, dot + 1).unwrap();
        let output = crate::diagnostics::check_completion(&uri, &repaired, &[]);
        assert!(!output.analysis.occurrences.is_empty());

        let items = dot_complete(result, &output.analysis, dot, ResolvePaths::default());

        assert_eq!(labels(&items), vec!["Err", "Ok"]);
    }

    #[test]
    fn enum_variant_prefix_filters_by_name() {
        let text = "fn main() {\n    let o = Option.So;\n}\n";
        let dot = text.find("Option.So").unwrap() + "Option".len();
        let analysis = plain_analysis(text, dot);

        let items = dot_complete(text, &analysis, dot, ResolvePaths::default());

        assert_eq!(labels(&items), vec!["Some"]);
    }

    #[test]
    fn imported_enum_variants_complete_after_type_name() {
        let dir = std::env::temp_dir().join(format!("zeen-lsp-enum-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lib.zn"),
            "pub enum Color {\n    red,\n    green: i32,\n}\n",
        )
        .unwrap();

        let text = "use lib;\n\nfn main() {\n    let c = Color.red;\n}\n";
        std::fs::write(dir.join("main.zn"), text).unwrap();
        let uri: tower_lsp_server::ls_types::Uri =
            format!("file://{}/main.zn", dir.display()).parse().unwrap();
        let output = crate::diagnostics::check(&uri, text, &[]);
        let errors = crate::diagnostics::only_errors(&output.diagnostics);
        assert!(errors.is_empty(), "sample must check cleanly: {:?}", errors);

        let dot = text.find("Color.red").unwrap() + "Color".len();
        let items = dot_complete(
            text,
            &output.analysis,
            dot,
            ResolvePaths {
                dir: Some(&dir),
                root: Some(&dir),
                std: None,
                packages: None,
            },
        );

        assert_eq!(labels(&items), vec!["red"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
    const DEFAULT_IFACE_SAMPLE: &str = "interface Shape {\n    fn area(*const self) i32;\n\n    fn name(*const self) []const char {\n        return \"shape\";\n    }\n}\n\nstruct Square {\n    side: i32,\n}\n\nimplement Shape : Square {\n    fn area(*const self) i32 {\n        return self.side * self.side;\n    }\n}\n\nfn main() {\n    let s = Square { .side = 3 };\n    s.\n}\n";

    fn dangling_analysis(text: &str, dot: usize) -> crate::analysis::Analysis {
        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-probe.zn".parse().unwrap();
        let repaired = repair_for_completion(text, dot + 1).unwrap();
        let output = crate::diagnostics::check(&uri, &repaired, &[]);

        assert!(
            !output.analysis.occurrences.is_empty(),
            "repaired text must analyze"
        );

        output.analysis
    }

    #[test]
    fn interface_defaults_complete_on_local_value() {
        let dot = DEFAULT_IFACE_SAMPLE.find("s.").unwrap() + 1;
        let analysis = dangling_analysis(DEFAULT_IFACE_SAMPLE, dot);
        let items = dot_complete(
            DEFAULT_IFACE_SAMPLE,
            &analysis,
            dot,
            ResolvePaths::default(),
        );
        let names = labels(&items);

        assert!(
            names.contains(&"name()"),
            "default must complete: {names:?}"
        );
        assert!(
            names.contains(&"area()"),
            "declared must complete: {names:?}"
        );
        assert!(
            !names.contains(&"side"),
            "private field stays hidden: {names:?}"
        );
    }

    #[test]
    fn private_fields_complete_in_named_init() {
        let text =
            "struct Rect {\n    w: i32,\n    h: i32,\n}\n\nfn main() {\n    let a = Rect { .\n}\n";
        let anchor = "Rect { .";
        let offset = text.find(anchor).unwrap() + anchor.len();

        assert!(struct_init_field_position(text, offset));

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-probe.zn".parse().unwrap();
        let repaired = repair_for_completion_semi(text, offset, 1, true).unwrap();
        let output = crate::diagnostics::check(&uri, &repaired, &[]);
        let items = struct_init_complete(text, &output.analysis, offset, ResolvePaths::default());

        assert_eq!(labels(&items), vec!["h", "w"]);
    }

    #[test]
    fn private_fields_complete_in_short_init() {
        let text = "struct Rect {\n    w: i32,\n    h: i32,\n}\n\nfn main() {\n    let a: Rect = .{ .\n}\n";
        let anchor = ".{ .";
        let offset = text.find(anchor).unwrap() + anchor.len();

        assert!(struct_init_field_position(text, offset));

        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-completion-probe.zn".parse().unwrap();
        let repaired = repair_for_completion_semi(text, offset, 1, true).unwrap();
        let output = crate::diagnostics::check(&uri, &repaired, &[]);
        let items = struct_init_complete(text, &output.analysis, offset, ResolvePaths::default());

        assert_eq!(labels(&items), vec!["h", "w"]);
    }

    const DEFAULT_IFACE_LIB: &str = "pub interface Shape {\n    pub fn area(*const self) i32;\n\n    pub fn name(*const self) []const char {\n        return \"shape\";\n    }\n}\n";

    #[test]
    fn imported_interface_defaults_complete_on_local_impl() {
        let dir = std::env::temp_dir().join(format!("zeen-lsp-iface-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.zn"), DEFAULT_IFACE_LIB).unwrap();

        let text = "use lib;\n\nstruct Local {\n    w: i32,\n}\n\nimplement Shape : Local {\n    pub fn area(*const self) i32 {\n        return self.w;\n    }\n}\n\nfn main() {\n    let s = Local { .w = 3 };\n    s.\n}\n";
        std::fs::write(dir.join("main.zn"), text).unwrap();
        let uri: tower_lsp_server::ls_types::Uri =
            format!("file://{}/main.zn", dir.display()).parse().unwrap();
        let dot = text.find("s.").unwrap() + 1;

        let repaired = repair_for_completion(text, dot + 1).unwrap();
        let output = crate::diagnostics::check(&uri, &repaired, &[]);
        let items = dot_complete(
            text,
            &output.analysis,
            dot,
            ResolvePaths {
                dir: Some(&dir),
                root: Some(&dir),
                std: None,
                packages: None,
            },
        );
        let names = labels(&items);

        assert!(
            names.contains(&"name()"),
            "default must complete: {names:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn imported_implement_defaults_complete() {
        let dir =
            std::env::temp_dir().join(format!("zeen-lsp-iface-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lib.zn"),
            format!(
                "{DEFAULT_IFACE_LIB}\npub struct Rect {{\n    pub w: i32,\n}}\n\nimplement Shape : Rect {{\n    pub fn area(*const self) i32 {{\n        return self.w;\n    }}\n}}\n"
            ),
        )
        .unwrap();

        let text = "use lib;\n\nfn main() {\n    let r = Rect { .w = 1 };\n    r.\n}\n";
        std::fs::write(dir.join("main.zn"), text).unwrap();
        let uri: tower_lsp_server::ls_types::Uri =
            format!("file://{}/main.zn", dir.display()).parse().unwrap();
        let dot = text.find("r.").unwrap() + 1;

        let repaired = repair_for_completion(text, dot + 1).unwrap();
        let output = crate::diagnostics::check(&uri, &repaired, &[]);
        let items = dot_complete(
            text,
            &output.analysis,
            dot,
            ResolvePaths {
                dir: Some(&dir),
                root: Some(&dir),
                std: None,
                packages: None,
            },
        );
        let names = labels(&items);

        assert!(
            names.contains(&"name()"),
            "default must complete: {names:?}"
        );
        assert!(
            names.contains(&"area()"),
            "override must complete: {names:?}"
        );
        assert_eq!(
            names.iter().filter(|name| **name == "area()").count(),
            1,
            "no duplicates: {names:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
