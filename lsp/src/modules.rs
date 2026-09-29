use std::{cell::RefCell, path::Path, rc::Rc, sync::Arc};

use zeen_ast::declarations::DeclarationKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModuleKind {
    Function,
    Struct,
    Enum,
    Interface,
    Const,
    Alias,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleMember {
    pub module: String,
    pub name: String,
    pub kind: ModuleKind,
}

pub fn use_imports(text: &str) -> Vec<String> {
    let mut imports = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim_start();

        if !trimmed.starts_with("use ") && !trimmed.starts_with("use\t") {
            continue;
        }

        let rest = trimmed
            .strip_prefix("use ")
            .or_else(|| trimmed.strip_prefix("use\t"))
            .unwrap_or("")
            .trim_start();

        let path = rest.split(';').next().unwrap_or("").trim();

        if !path.is_empty() {
            imports.push(path.to_string());
        }
    }

    imports
}

pub fn members_of_use(open_dir: &Path, std_root: Option<&Path>, import: &str) -> Vec<ModuleMember> {
    if import.starts_with("core.") {
        let Some(content) = zeen_driver::CORE_FILES
            .iter()
            .find(|file| file.name == import)
            .map(|file| file.value)
        else {
            return Vec::new();
        };

        return members_of_source(content, import);
    }

    let path = if let Some(rest) = import.strip_prefix("std.") {
        std_root.map(|root| root.join(rest.replace('.', "/") + ".zn"))
    } else if !import.contains('.') {
        Some(open_dir.join(import.to_string() + ".zn"))
    } else {
        None
    };

    let Some(path) = path else {
        return Vec::new();
    };

    let Ok(content) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };

    members_of_source(&content, import)
}

fn members_of_source(content: &str, module: &str) -> Vec<ModuleMember> {
    let filename = Rc::new(format!("{module}.zn"));
    let source = Arc::new(content.to_string());
    let interner = Rc::new(RefCell::new(lasso::Rodeo::default()));
    let bump = bumpalo::Bump::new();
    let mut tokens = zeen_lexer::tokenize(content);

    let mut parser = zeen_parser::Parser::new(
        Rc::clone(&filename),
        Arc::clone(&source),
        &mut tokens,
        &bump,
        Rc::clone(&interner),
    );

    let Ok(program) = parser.parse_program() else {
        return Vec::new();
    };

    let interner = interner.borrow();
    let mut members = Vec::new();

    for decl in program {
        let (name, kind, is_pub) = match &decl.kind {
            DeclarationKind::FnDecl { name, is_pub, .. } => (name.0, ModuleKind::Function, *is_pub),
            DeclarationKind::StructDecl { name, is_pub, .. } => {
                (name.0, ModuleKind::Struct, *is_pub)
            }
            DeclarationKind::EnumDecl { name, is_pub, .. } => (name.0, ModuleKind::Enum, *is_pub),
            DeclarationKind::InterfaceDecl { name, is_pub, .. } => {
                (name.0, ModuleKind::Interface, *is_pub)
            }
            DeclarationKind::ExternVar { name, is_pub, .. }
            | DeclarationKind::GlobalVar { name, is_pub, .. } => {
                (name.0, ModuleKind::Const, *is_pub)
            }
            DeclarationKind::Alias(alias) => (alias.name.0, ModuleKind::Alias, alias.is_pub),
            _ => continue,
        };

        if !is_pub {
            continue;
        }

        members.push(ModuleMember {
            module: module.to_string(),
            name: interner.resolve(&name).to_string(),
            kind,
        });
    }

    members.sort_by(|a, b| a.name.cmp(&b.name));
    members
}
