use std::{cell::RefCell, path::Path, rc::Rc, sync::Arc};

use zeen_ast::declarations::{DeclarationKind, EnumVariantPayload};

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
    pub is_static: bool,
}

fn fn_takes_self(params: &[zeen_ast::declarations::FnParam<'_>]) -> bool {
    let Some(first) = params.first() else {
        return false;
    };

    let mut current = &first.ty.kind;

    loop {
        match current {
            zeen_ast::types::TypeKind::SelfType => return true,
            zeen_ast::types::TypeKind::Const(inner)
            | zeen_ast::types::TypeKind::SinglePointer(inner) => current = &inner.kind,
            _ => return false,
        }
    }
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
    project_root: &Path,
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

    let segments: Vec<&str> = import.split('.').collect();

    if segments.is_empty() {
        return None;
    }

    let (base_dir, rest): (std::path::PathBuf, &[&str]) = match segments[0] {
        "std" => (std_root?.to_path_buf(), &segments[1..]),
        "root" => (project_root.to_path_buf(), &segments[1..]),
        "super" => (
            open_dir.parent().unwrap_or(open_dir).to_path_buf(),
            &segments[1..],
        ),
        _ => (open_dir.to_path_buf(), &segments[..]),
    };

    let mut dir_path = base_dir;

    for seg in rest {
        if seg.is_empty() {
            return None;
        }

        dir_path.push(seg);
    }

    let file_path = file_or_index(&dir_path)?;
    let content = std::fs::read_to_string(&file_path).ok()?;

    Some((import.to_string(), content))
}

fn file_or_index(dir_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut file_path = dir_path.to_path_buf();
    file_path.set_extension("zn");

    if file_path.is_file() {
        return Some(file_path);
    }

    let index_path = dir_path.join("index.zn");

    if index_path.is_file() {
        return Some(index_path);
    }

    None
}

pub fn members_of_use(
    open_dir: &Path,
    project_root: &Path,
    std_root: Option<&Path>,
    import: &str,
) -> Vec<ModuleMember> {
    let Some((module, content)) = resolve_import(open_dir, project_root, std_root, import) else {
        return Vec::new();
    };

    members_of_source(&content, &module)
}

pub fn type_members(
    open_dir: &Path,
    project_root: &Path,
    std_root: Option<&Path>,
    imports: &[String],
    type_name: &str,
) -> Vec<ModuleMember> {
    let mut members = Vec::new();

    for import in imports {
        let Some((module, content)) = resolve_import(open_dir, project_root, std_root, import)
        else {
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
            is_static: false,
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
                        is_static: false,
                    });
                }

                for method in *methods {
                    if let DeclarationKind::FnDecl {
                        name,
                        params,
                        is_pub: true,
                        ..
                    } = &method.kind
                    {
                        members.push(ModuleMember {
                            module: module.to_string(),
                            name: resolve(&name.0),
                            kind: ModuleKind::Method,
                            is_static: !fn_takes_self(params),
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
                        is_static: true,
                    });
                }

                for method in *methods {
                    if let DeclarationKind::FnDecl {
                        name,
                        params,
                        is_pub: true,
                        ..
                    } = &method.kind
                    {
                        members.push(ModuleMember {
                            module: module.to_string(),
                            name: resolve(&name.0),
                            kind: ModuleKind::Method,
                            is_static: !fn_takes_self(params),
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
                        name,
                        params,
                        is_pub: true,
                        ..
                    } = &method.kind
                    {
                        members.push(ModuleMember {
                            module: module.to_string(),
                            name: resolve(&name.0),
                            kind: ModuleKind::Method,
                            is_static: !fn_takes_self(params),
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

pub fn variant_payload_fields(
    open_dir: &Path,
    project_root: &Path,
    std_root: Option<&Path>,
    imports: &[String],
    variant_name: &str,
) -> Vec<ModuleMember> {
    let mut fields = Vec::new();

    for import in imports {
        let Some((module, content)) = resolve_import(open_dir, project_root, std_root, import)
        else {
            continue;
        };

        let filename = Rc::new(format!("{module}.zn"));
        let source = Arc::new(content.clone());
        let interner = Rc::new(RefCell::new(lasso::Rodeo::default()));
        let bump = bumpalo::Bump::new();
        let mut tokens = zeen_lexer::tokenize(&content);

        let mut parser = zeen_parser::Parser::new(
            Rc::clone(&filename),
            Arc::clone(&source),
            &mut tokens,
            &bump,
            Rc::clone(&interner),
        );

        let Ok(program) = parser.parse_program() else {
            continue;
        };

        let interner = interner.borrow();
        let resolve = |spur: &lasso::Spur| interner.resolve(spur).to_string();

        for decl in program {
            let DeclarationKind::EnumDecl { variants, .. } = &decl.kind else {
                continue;
            };

            for variant in *variants {
                if resolve(&variant.name) != variant_name {
                    continue;
                }

                if let Some(EnumVariantPayload::Anonymous(payload)) = variant.payload {
                    for field in payload {
                        fields.push(ModuleMember {
                            module: module.to_string(),
                            name: resolve(&field.name),
                            kind: ModuleKind::Field,
                            is_static: false,
                        });
                    }
                }
            }
        }
    }

    fields.sort_by(|a, b| a.name.cmp(&b.name));
    fields.dedup_by(|a, b| a.name == b.name);
    fields
}
