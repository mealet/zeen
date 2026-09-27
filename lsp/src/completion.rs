use std::collections::HashSet;

use tower_lsp_server::ls_types::{CompletionItem, CompletionItemKind};
use zeen_hir::HirMacroKind;
use zeen_lexer::token::{CompilerKeyword, CompilerType};

use crate::analysis::{Analysis, MemberKind, Role};

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

pub fn complete(text: &str, analysis: &Analysis, offset: usize) -> Vec<CompletionItem> {
    let prefix = ident_prefix(text, offset);
    let mut seen: HashSet<String> = HashSet::new();
    let mut items = Vec::new();

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

        items.push(CompletionItem {
            label: name.to_string(),
            kind: Some(role_kind(occurrence.role)),
            detail: occurrence.ty.clone(),
            ..Default::default()
        });
    }

    items
}

fn member_kind(kind: &MemberKind) -> CompletionItemKind {
    match kind {
        MemberKind::Method => CompletionItemKind::METHOD,
        MemberKind::Field => CompletionItemKind::FIELD,
        MemberKind::Variant => CompletionItemKind::ENUM_MEMBER,
    }
}

fn normalize_type_name(ty: &str) -> Option<&str> {
    let mut name = ty.trim();

    while let Some(rest) = name.strip_prefix('*') {
        name = rest.trim_start();
    }

    if let Some(bracket) = name.find(['[', '(']) {
        name = &name[..bracket];
    }

    if name.is_empty() {
        return None;
    }

    Some(name)
}

pub fn dot_complete(text: &str, analysis: &Analysis, dot_offset: usize) -> Vec<CompletionItem> {
    let bytes = text.as_bytes();
    let mut start = dot_offset;

    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }

    if start == dot_offset {
        return Vec::new();
    }

    let Some(receiver) = analysis.at(start) else {
        return Vec::new();
    };

    let Some(ty) = &receiver.ty else {
        return Vec::new();
    };

    let Some(name) = normalize_type_name(ty) else {
        return Vec::new();
    };

    let Some(def) = analysis.occurrences.iter().find_map(|occurrence| {
        if occurrence.role == Role::Type
            && text.get(occurrence.offset..occurrence.offset + occurrence.len) == Some(name)
        {
            occurrence.def
        } else {
            None
        }
    }) else {
        return Vec::new();
    };

    let Some(members) = analysis.members.get(&def) else {
        return Vec::new();
    };

    members
        .iter()
        .map(|member| CompletionItem {
            label: member.name.clone(),
            kind: Some(member_kind(&member.kind)),
            ..Default::default()
        })
        .collect()
}
