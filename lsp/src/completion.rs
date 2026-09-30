use std::collections::HashSet;

use tower_lsp_server::ls_types::{CompletionItem, CompletionItemKind, InsertTextFormat};
use zeen_hir::HirMacroKind;
use zeen_lexer::token::{CompilerKeyword, CompilerType};

use crate::analysis::{Analysis, MemberKind, Role};
use crate::modules::ModuleKind;

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
    open_dir: Option<&std::path::Path>,
    std_root: Option<&std::path::Path>,
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

        items.push(CompletionItem {
            label: name.to_string(),
            kind: Some(role_kind(occurrence.role)),
            detail: occurrence.ty.clone(),
            ..Default::default()
        });
    }

    if let Some(dir) = open_dir {
        for import in crate::modules::use_imports(text) {
            for member in crate::modules::members_of_use(dir, std_root, &import) {
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
    }
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

    let name: &str = if receiver.role == Role::Type {
        let Some(name) = text.get(receiver.offset..receiver.offset + receiver.len) else {
            return Vec::new();
        };
        name
    } else {
        let Some(ty) = &receiver.ty else {
            return Vec::new();
        };
        let Some(name) = normalize_type_name(ty) else {
            return Vec::new();
        };
        name
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
