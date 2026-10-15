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

pub(crate) fn resolve_import(
    open_dir: &Path,
    project_root: &Path,
    std_root: Option<&Path>,
    packages: &[(String, std::path::PathBuf)],
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
        name => match packages.iter().rev().find(|(pkg, _)| pkg == name) {
            Some((_, dir)) => (dir.clone(), &segments[1..]),
            None => (open_dir.to_path_buf(), &segments[..]),
        },
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
    packages: &[(String, std::path::PathBuf)],
    import: &str,
) -> Vec<ModuleMember> {
    let Some((module, content)) =
        resolve_import(open_dir, project_root, std_root, packages, import)
    else {
        return Vec::new();
    };

    members_of_source(&content, &module)
}

pub fn type_members(
    open_dir: &Path,
    project_root: &Path,
    std_root: Option<&Path>,
    packages: &[(String, std::path::PathBuf)],
    imports: &[String],
    type_name: &str,
) -> Vec<ModuleMember> {
    let mut members = Vec::new();

    for import in imports {
        let Some((module, content)) =
            resolve_import(open_dir, project_root, std_root, packages, import)
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
    packages: &[(String, std::path::PathBuf)],
    imports: &[String],
    variant_name: &str,
) -> Vec<ModuleMember> {
    let mut fields = Vec::new();

    for import in imports {
        let Some((module, content)) =
            resolve_import(open_dir, project_root, std_root, packages, import)
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

pub fn method_source_sig(
    open_dir: &Path,
    project_root: &Path,
    std_root: Option<&Path>,
    packages: &[(String, std::path::PathBuf)],
    imports: &[String],
    type_name: &str,
    method_name: &str,
) -> Option<String> {
    for import in imports {
        let Some((_, content)) = resolve_import(open_dir, project_root, std_root, packages, import)
        else {
            continue;
        };

        if let Some(signature) = method_sig_in_source(&content, type_name, method_name) {
            return Some(signature);
        }
    }

    None
}

fn method_sig_in_source(content: &str, type_name: &str, method_name: &str) -> Option<String> {
    let filename = Rc::new(String::from("lookup.zn"));
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

    let program = parser.parse_program().ok()?;
    let interner = interner.borrow();
    let resolve = |spur: &lasso::Spur| interner.resolve(spur).to_string();

    for decl in program {
        let methods = match &decl.kind {
            DeclarationKind::StructDecl { name, methods, .. }
            | DeclarationKind::EnumDecl { name, methods, .. } => {
                if resolve(&name.0) != type_name {
                    continue;
                }

                methods
            }
            DeclarationKind::ImplementDecl {
                object, methods, ..
            } => {
                if resolve(&object.0) != type_name {
                    continue;
                }

                methods
            }
            _ => continue,
        };

        for method in *methods {
            let DeclarationKind::FnDecl {
                name,
                generics,
                params,
                return_type,
                ..
            } = &method.kind
            else {
                continue;
            };

            if resolve(&name.0) != method_name {
                continue;
            }

            let mut code = format!("fn {}", resolve(&name.0));

            if let Some(generics) = generics {
                let mut bounds = Vec::with_capacity(generics.len());

                for generic in *generics {
                    let mut bound = resolve(&generic.name.0);

                    if let Some(interfaces) = generic.interfaces {
                        let names: Vec<String> =
                            interfaces.iter().map(|(spur, _)| resolve(spur)).collect();

                        if !names.is_empty() {
                            bound.push_str(": ");
                            bound.push_str(&names.join(" + "));
                        }
                    }

                    bounds.push(bound);
                }

                code.push('[');
                code.push_str(&bounds.join(", "));
                code.push(']');
            }

            let mut args = Vec::with_capacity(params.len());

            for param in *params {
                let start = param.span.offset().min(content.len());
                let end = start.saturating_add(param.span.len()).min(content.len());

                if let Some(spelling) = content.get(start..end).filter(|text| !text.is_empty()) {
                    args.push(spelling.to_string());
                }
            }

            code.push('(');
            code.push_str(&args.join(", "));
            code.push(')');

            if let Some(ret) = return_type {
                let start = ret.span.offset().min(content.len());
                let end = start.saturating_add(ret.span.len()).min(content.len());

                if let Some(spelling) = content.get(start..end).filter(|text| !text.is_empty()) {
                    code.push(' ');
                    code.push_str(spelling);
                }
            }

            return Some(code);
        }
    }

    None
}

pub fn prefixed_modules(base: &Path, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    collect_prefixed(base, base, prefix, &mut out, 0);
    out.sort();
    out
}

fn collect_prefixed(base: &Path, dir: &Path, prefix: &str, out: &mut Vec<String>, depth: usize) {
    if depth > 6 {
        return;
    }

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        let Some(name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.starts_with('.'))
        else {
            continue;
        };

        if path.is_dir() {
            if name == "target" {
                continue;
            }

            collect_prefixed(base, &path, prefix, out, depth + 1);
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) != Some("zn") {
            continue;
        }

        let Ok(relative) = path.strip_prefix(base) else {
            continue;
        };

        let mut dotted = prefix.to_string();

        for component in relative.components() {
            let Some(text) = component.as_os_str().to_str() else {
                continue;
            };

            dotted.push('.');
            dotted.push_str(text);
        }

        let Some(candidate) = dotted.strip_suffix(".zn") else {
            continue;
        };

        out.push(candidate.to_string());

        if let Some(indexless) = candidate.strip_suffix(".index") {
            out.push(indexless.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("zeen-lsp-prefixed-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub").join("deep")).unwrap();
        std::fs::create_dir_all(dir.join("pkg")).unwrap();
        std::fs::write(dir.join("lib.zn"), "pub fn f() i32 {\n    return 1;\n}\n").unwrap();
        std::fs::write(
            dir.join("sub").join("lib.zn"),
            "pub fn g() i32 {\n    return 2;\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("pkg").join("index.zn"),
            "pub fn h() i32 {\n    return 3;\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "skip me\n").unwrap();
        dir
    }

    #[test]
    fn lists_nested_with_prefix() {
        let dir = layout("nested");
        let modules = prefixed_modules(&dir, "super");

        assert!(modules.contains(&"super.lib".to_string()));
        assert!(modules.contains(&"super.sub.lib".to_string()));
        assert!(modules.contains(&"super.pkg.index".to_string()));
        assert!(modules.contains(&"super.pkg".to_string()));
        assert!(!modules.iter().any(|module| module.contains("notes")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_dir_gives_empty() {
        let dir =
            std::env::temp_dir().join(format!("zeen-lsp-prefixed-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        assert!(prefixed_modules(&dir, "root").is_empty());
    }
}
