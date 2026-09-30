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
    Method,
    Field,
    Variant,
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

fn resolve_import(
    open_dir: &Path,
    std_root: Option<&Path>,
    import: &str,
) -> Option<(String, String)> {
    if import.starts_with("core.") {
        let content = zeen_driver::CORE_FILES
            .iter()
            .find(|file| file.name == import)
            .map(|file| file.value)?;

        return Some((import.to_string(), content.to_string()));
    }

    let path = if let Some(rest) = import.strip_prefix("std.") {
        std_root.map(|root| root.join(rest.replace('.', "/") + ".zn"))
    } else if !import.contains('.') {
        Some(open_dir.join(import.to_string() + ".zn"))
    } else {
        None
    };

    let path = path?;
    let content = std::fs::read_to_string(&path).ok()?;

    Some((import.to_string(), content))
}

pub fn members_of_use(open_dir: &Path, std_root: Option<&Path>, import: &str) -> Vec<ModuleMember> {
    let Some((module, content)) = resolve_import(open_dir, std_root, import) else {
        return Vec::new();
    };

    members_of_source(&content, &module)
}

pub fn type_members(
    open_dir: &Path,
    std_root: Option<&Path>,
    imports: &[String],
    type_name: &str,
) -> Vec<ModuleMember> {
    let mut members = Vec::new();

    for import in imports {
        let Some((module, content)) = resolve_import(open_dir, std_root, import) else {
            continue;
        };

        members.extend(members_of_type(&content, &module, type_name));
    }

    members.sort_by(|a, b| a.name.cmp(&b.name));
    members
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

fn members_of_type(content: &str, module: &str, type_name: &str) -> Vec<ModuleMember> {
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
    let resolve = |spur: &lasso::Spur| interner.resolve(spur).to_string();
    let mut members = Vec::new();

    for decl in program {
        match &decl.kind {
            DeclarationKind::StructDecl {
                name,
                fields,
                methods,
                ..
            } => {
                if resolve(&name.0) != type_name {
                    continue;
                }

                for field in *fields {
                    if !field.is_pub {
                        continue;
                    }

                    members.push(ModuleMember {
                        module: module.to_string(),
                        name: resolve(&field.name),
                        kind: ModuleKind::Field,
                    });
                }

                for method in *methods {
                    if let DeclarationKind::FnDecl {
                        name, is_pub: true, ..
                    } = &method.kind
                    {
                        members.push(ModuleMember {
                            module: module.to_string(),
                            name: resolve(&name.0),
                            kind: ModuleKind::Method,
                        });
                    }
                }
            }
            DeclarationKind::EnumDecl {
                name,
                variants,
                methods,
                ..
            } => {
                if resolve(&name.0) != type_name {
                    continue;
                }

                for variant in *variants {
                    members.push(ModuleMember {
                        module: module.to_string(),
                        name: resolve(&variant.name),
                        kind: ModuleKind::Variant,
                    });
                }

                for method in *methods {
                    if let DeclarationKind::FnDecl {
                        name, is_pub: true, ..
                    } = &method.kind
                    {
                        members.push(ModuleMember {
                            module: module.to_string(),
                            name: resolve(&name.0),
                            kind: ModuleKind::Method,
                        });
                    }
                }
            }
            DeclarationKind::ImplementDecl {
                object, methods, ..
            } => {
                if resolve(&object.0) != type_name {
                    continue;
                }

                for method in *methods {
                    if let DeclarationKind::FnDecl {
                        name, is_pub: true, ..
                    } = &method.kind
                    {
                        members.push(ModuleMember {
                            module: module.to_string(),
                            name: resolve(&name.0),
                            kind: ModuleKind::Method,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    members.sort_by(|a, b| a.name.cmp(&b.name));
    members
}
