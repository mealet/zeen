use crate::analysis::{Analysis, Member, MemberKind, Occurrence, Role};
use crate::completion::{ResolvePaths, normalize_type_name};
pub struct HoverResult {
    pub markdown: String,
    pub offset: usize,
    pub len: usize,
}

pub fn hover_markdown(
    text: &str,
    analysis: &Analysis,
    offset: usize,
    paths: ResolvePaths<'_>,
    filename: &str,
) -> Option<HoverResult> {
    let occurrence = analysis.at(offset)?;
    let name = text.get(occurrence.offset..occurrence.offset + occurrence.len)?;
    let key = (occurrence.offset, occurrence.len);

    if let Some(fn_def) = method_at(analysis, key) {
        return Some(method_call_hover(
            text, analysis, name, fn_def, occurrence, paths, filename,
        ));
    }

    if let Some(target) = analysis.field_targets.get(&key) {
        return Some(field_access_hover(
            text, analysis, name, target, occurrence, filename,
        ));
    }

    match occurrence.role {
        Role::Method => Some(method_decl_hover(
            text, analysis, name, occurrence, filename,
        )),
        Role::Function => Some(function_or_method_hover(
            text, analysis, name, occurrence, filename,
        )),
        Role::Property => Some(property_hover(text, analysis, name, occurrence, filename)),
        Role::Variable | Role::Parameter => Some(binding_hover(text, name, occurrence, filename)),
        Role::Type => Some(type_hover(text, name, occurrence, filename)),
        Role::EnumMember => Some(variant_hover(text, analysis, name, occurrence, filename)),
    }
}

fn assemble(code: &str, info: &[String], occurrence: &Occurrence) -> (String, usize, usize) {
    let mut markdown = code_block(code);

    if !info.is_empty() {
        markdown.push_str("\n----\n");
        markdown.push_str(&info.join("\n"));
    }

    (markdown, occurrence.offset, occurrence.len)
}

fn code_block(code: &str) -> String {
    format!("```zeen\n{code}\n```")
}

fn defined_at(text: &str, occurrence: &Occurrence, filename: &str) -> Option<String> {
    if occurrence.target_file.is_some() {
        return occurrence
            .target_module
            .as_deref()
            .map(|module| format!("defined in `{module}`"));
    }

    let anchor = occurrence
        .target_offset
        .filter(|_| occurrence.target_len.is_some())
        .or_else(|| occurrence.def.map(|_| occurrence.offset))?;

    let line = text[..anchor.min(text.len())]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;

    Some(format!("defined at {filename}:{line}"))
}

pub(crate) fn method_at(analysis: &Analysis, key: (usize, usize)) -> Option<zeen_resolve::DefId> {
    if let Some(def) = analysis.method_calls.get(&key).copied() {
        return Some(def);
    }

    analysis
        .method_calls
        .iter()
        .filter(|((start, len), _)| {
            (*start, *len) != key && *start < key.0 && key.0 + key.1 <= start + len
        })
        .min_by_key(|((_, len), _)| *len)
        .map(|(_, def)| *def)
}

fn find_member(
    analysis: &Analysis,
    def: zeen_resolve::DefId,
) -> Option<(zeen_resolve::DefId, &Member)> {
    for (owner, members) in &analysis.members {
        for member in members {
            if member.def == def {
                return Some((*owner, member));
            }
        }
    }

    None
}

fn owner_name<'a>(
    text: &'a str,
    analysis: &Analysis,
    owner: zeen_resolve::DefId,
) -> Option<&'a str> {
    analysis.occurrences.iter().find_map(|occurrence| {
        if occurrence.role == Role::Type && occurrence.def == Some(owner) {
            text.get(occurrence.offset..occurrence.offset + occurrence.len)
        } else {
            None
        }
    })
}

fn fn_generics_part(analysis: &Analysis, def: zeen_resolve::DefId) -> String {
    match analysis.fn_generics.get(&def) {
        Some(generics) if !generics.is_empty() => format!("[{}]", generics.join(", ")),
        _ => String::new(),
    }
}

fn fn_code(
    analysis: &Analysis,
    fn_def: zeen_resolve::DefId,
    name: &str,
    ty: Option<&str>,
) -> String {
    if let Some(source) = analysis.fn_source_sig.get(&fn_def) {
        return source.clone();
    }

    let generics = fn_generics_part(analysis, fn_def);

    if let Some(signature) = analysis.fn_signatures.get(&fn_def) {
        return format!("fn {name}{generics}{signature}");
    }

    let spelling = analysis.method_self.get(&fn_def).map(String::as_str);
    let fallback =
        find_member(analysis, fn_def).and_then(|(_, member)| member.signature.as_deref());

    render_fn(name, &generics, fallback.or(ty), spelling)
}

fn render_fn(
    name: &str,
    generics: &str,
    ty_display: Option<&str>,
    self_spelling: Option<&str>,
) -> String {
    match ty_display.and_then(|signature| signature.strip_prefix("fn")) {
        Some(rest) => {
            let rest = match self_spelling {
                Some(spelling) => replace_first_param(rest, spelling),
                None => rest.to_string(),
            };

            format!("fn {name}{generics}{rest}")
        }
        None => format!("fn {name}{generics}(...)"),
    }
}

fn replace_first_param(rest: &str, spelling: &str) -> String {
    let bytes = rest.as_bytes();

    if bytes.first() != Some(&b'(') {
        return rest.to_string();
    }

    let mut depth: usize = 0;
    let mut end = None;

    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'(' | b'[' => depth += 1,
            b')' | b']' => {
                depth = depth.saturating_sub(1);

                if depth == 0 {
                    end = Some(index);
                    break;
                }
            }
            b',' if depth == 1 => {
                end = Some(index);
                break;
            }
            _ => {}
        }
    }

    match end {
        Some(index) => format!("({spelling}{}", &rest[index..]),
        None => rest.to_string(),
    }
}

fn method_kind_label(is_static: Option<bool>) -> &'static str {
    match is_static {
        Some(true) => "static method",
        Some(false) => "instance method",
        None => "method",
    }
}

fn receiver_ty_before(text: &str, analysis: &Analysis, field_start: usize) -> Option<String> {
    if field_start == 0 || text.as_bytes().get(field_start - 1) != Some(&b'.') {
        return None;
    }

    let mut start = field_start - 1;

    while start > 0 && {
        let byte = text.as_bytes()[start - 1];
        byte.is_ascii_alphanumeric() || byte == b'_'
    } {
        start -= 1;
    }

    if start == field_start - 1 {
        return None;
    }

    if let Some(ty) = analysis
        .access_types
        .get(&(start, field_start - 1 - start))
        .and_then(|ty| normalize_type_name(ty))
    {
        return Some(ty);
    }

    analysis
        .at(start)
        .and_then(|receiver| receiver.ty.as_deref())
        .and_then(normalize_type_name)
}

fn imported_method_static(
    text: &str,
    analysis: &Analysis,
    paths: ResolvePaths<'_>,
    field_start: usize,
    method: &str,
) -> Option<bool> {
    if field_start == 0 || text.as_bytes().get(field_start - 1) != Some(&b'.') {
        return None;
    }

    let mut start = field_start - 1;

    while start > 0 && {
        let byte = text.as_bytes()[start - 1];
        byte.is_ascii_alphanumeric() || byte == b'_'
    } {
        start -= 1;
    }

    if start == field_start - 1 {
        return None;
    }

    if let Some(receiver) = analysis.at(start)
        && receiver.role == Role::Type
        && let Some(name) = text.get(receiver.offset..receiver.offset + receiver.len)
    {
        return lookup_imported_static(text, paths, name, method);
    }

    let ty = receiver_ty_before(text, analysis, field_start)?;

    lookup_imported_static(text, paths, &ty, method)
}

fn lookup_imported_static(
    text: &str,
    paths: ResolvePaths<'_>,
    ty: &str,
    method: &str,
) -> Option<bool> {
    let (dir, root) = match (paths.dir, paths.project()) {
        (Some(dir), Some(root)) => (dir, root),
        _ => return None,
    };

    crate::modules::type_members(
        dir,
        root,
        paths.std,
        paths.packages(),
        &crate::modules::use_imports(text),
        ty,
    )
    .into_iter()
    .find(|member| member.kind == crate::modules::ModuleKind::Method && member.name == method)
    .map(|member| member.is_static)
}

fn method_call_hover(
    text: &str,
    analysis: &Analysis,
    name: &str,
    fn_def: zeen_resolve::DefId,
    occurrence: &Occurrence,
    paths: ResolvePaths<'_>,
    filename: &str,
) -> HoverResult {
    let key = (occurrence.offset, occurrence.len);
    let mut info = Vec::new();

    let code = if let Some((owner, member)) = find_member(analysis, fn_def) {
        let label = method_kind_label(Some(member.is_static));
        let owner_text = owner_name(text, analysis, owner)
            .map(|owner| format!("{label} of `{owner}`"))
            .unwrap_or_else(|| label.to_string());
        info.push(owner_text);

        fn_code(analysis, fn_def, name, None)
    } else {
        let dotted = key.0 > 0 && text.as_bytes().get(key.0 - 1) == Some(&b'.');

        if !dotted {
            info.push("function".to_string());
        } else {
            let is_static = imported_method_static(text, analysis, paths, key.0, name);
            info.push(method_kind_label(is_static).to_string());
        }

        fn_code(analysis, fn_def, name, occurrence.ty.as_deref())
    };

    if let Some(location) = defined_at(text, occurrence, filename) {
        info.push(location);
    }

    let (markdown, offset, len) = assemble(&code, &info, occurrence);

    HoverResult {
        markdown,
        offset,
        len,
    }
}

fn field_access_hover(
    text: &str,
    analysis: &Analysis,
    name: &str,
    target: &crate::analysis::FieldTarget,
    occurrence: &Occurrence,
    filename: &str,
) -> HoverResult {
    if let Some((owner, _)) = find_member(analysis, target.field_def)
        .filter(|(_, member)| member.kind == crate::analysis::MemberKind::Variant)
    {
        let mut info = vec!["enum member".to_string()];
        let code = owner_name(text, analysis, owner)
            .map(|owner| format!("{owner}.{name}"))
            .unwrap_or_else(|| name.to_string());

        if let Some(location) = defined_at(text, occurrence, filename) {
            info.push(location);
        }

        let (markdown, offset, len) = assemble(&code, &info, occurrence);

        return HoverResult {
            markdown,
            offset,
            len,
        };
    }

    let code = if target.is_pub {
        format!("pub {name}: {}", target.ty)
    } else {
        format!("{name}: {}", target.ty)
    };

    let info = defined_at(text, occurrence, filename)
        .map(|location| vec![location])
        .unwrap_or_default();

    let (markdown, offset, len) = assemble(&code, &info, occurrence);

    HoverResult {
        markdown,
        offset,
        len,
    }
}

fn method_decl_hover(
    text: &str,
    analysis: &Analysis,
    name: &str,
    occurrence: &Occurrence,
    filename: &str,
) -> HoverResult {
    let mut info = Vec::new();

    let code = occurrence.def.map_or_else(
        || render_fn(name, "", None, None),
        |def| {
            if let Some((owner, member)) = find_member(analysis, def) {
                let label = method_kind_label(Some(member.is_static));
                let owner_text = owner_name(text, analysis, owner)
                    .map(|owner| format!("{label} of `{owner}`"))
                    .unwrap_or_else(|| label.to_string());
                info.push(owner_text);

                fn_code(analysis, def, name, None)
            } else {
                info.push(method_kind_label(None).to_string());
                fn_code(analysis, def, name, None)
            }
        },
    );

    if let Some(location) = defined_at(text, occurrence, filename) {
        info.push(location);
    }

    let (markdown, offset, len) = assemble(&code, &info, occurrence);

    HoverResult {
        markdown,
        offset,
        len,
    }
}

fn function_or_method_hover(
    text: &str,
    analysis: &Analysis,
    name: &str,
    occurrence: &Occurrence,
    filename: &str,
) -> HoverResult {
    let is_method = occurrence.def.is_some_and(|def| {
        find_member(analysis, def).is_some_and(|(_, member)| member.kind == MemberKind::Method)
    });

    if is_method {
        method_decl_hover(text, analysis, name, occurrence, filename)
    } else {
        function_hover(text, analysis, name, occurrence, filename)
    }
}

fn function_hover(
    text: &str,
    analysis: &Analysis,
    name: &str,
    occurrence: &Occurrence,
    filename: &str,
) -> HoverResult {
    let mut info = vec!["function".to_string()];

    let code = occurrence.def.map_or_else(
        || render_fn(name, "", None, None),
        |def| fn_code(analysis, def, name, occurrence.ty.as_deref()),
    );

    if let Some(location) = defined_at(text, occurrence, filename) {
        info.push(location);
    }

    let (markdown, offset, len) = assemble(&code, &info, occurrence);

    HoverResult {
        markdown,
        offset,
        len,
    }
}

fn property_hover(
    text: &str,
    analysis: &Analysis,
    name: &str,
    occurrence: &Occurrence,
    filename: &str,
) -> HoverResult {
    let mut info = Vec::new();

    let field = occurrence
        .def
        .and_then(|def| find_member(analysis, def))
        .filter(|(_, member)| member.kind == crate::analysis::MemberKind::Field);

    if let Some((_, member)) = field {
        let code = member
            .signature
            .as_deref()
            .map(|ty| {
                if member.is_pub {
                    format!("pub {name}: {ty}")
                } else {
                    format!("{name}: {ty}")
                }
            })
            .unwrap_or_else(|| name.to_string());
        info.push("struct field".to_string());

        if let Some(location) = defined_at(text, occurrence, filename) {
            info.push(location);
        }

        let (markdown, offset, len) = assemble(&code, &info, occurrence);

        return HoverResult {
            markdown,
            offset,
            len,
        };
    }

    let payload_ty = variant_parent_before(text, occurrence.offset, occurrence.len).and_then(
        |(parent, variant)| {
            analysis.enum_payloads.get(&parent).and_then(|variants| {
                variants.iter().find_map(|(candidate, payload)| {
                    (candidate == &variant).then(|| payload.clone()).flatten()
                })
            })
        },
    );

    if payload_ty.is_some() {
        info.push("enum member".to_string());
    }

    let code = occurrence
        .def
        .and_then(|def| find_member(analysis, def))
        .and_then(|(_, member)| {
            member.signature.as_deref().map(|ty| {
                if member.is_pub {
                    format!("pub {name}: {ty}")
                } else {
                    format!("{name}: {ty}")
                }
            })
        })
        .or_else(|| payload_ty.map(|ty| format!("{name}: {ty}")))
        .unwrap_or_else(|| match &occurrence.ty {
            Some(ty) => format!("{name}: {ty}"),
            None => name.to_string(),
        });

    if let Some(caps) = occurrence.caps.as_deref() {
        info.push(caps.to_string());
    }

    if let Some(location) = defined_at(text, occurrence, filename) {
        info.push(location);
    }

    let (markdown, offset, len) = assemble(&code, &info, occurrence);

    HoverResult {
        markdown,
        offset,
        len,
    }
}

fn binding_hover(text: &str, name: &str, occurrence: &Occurrence, filename: &str) -> HoverResult {
    let prefix = match occurrence.role {
        Role::Variable => match occurrence.is_const {
            Some(true) => "const ",
            _ => "let ",
        },
        _ => "",
    };

    let code = match &occurrence.ty {
        Some(ty) => format!("{prefix}{name}: {ty}"),
        None => format!("{prefix}{name}"),
    };

    let mut info = Vec::new();

    if let Some(caps) = occurrence.caps.as_deref() {
        info.push(caps.to_string());
    }

    if let Some(location) = defined_at(text, occurrence, filename) {
        info.push(location);
    }

    let (markdown, offset, len) = assemble(&code, &info, occurrence);

    HoverResult {
        markdown,
        offset,
        len,
    }
}

fn type_hover(text: &str, name: &str, occurrence: &Occurrence, filename: &str) -> HoverResult {
    let mut info = Vec::new();

    if let Some(caps) = occurrence.caps.as_deref() {
        info.push(caps.to_string());
    }

    if let Some(location) = defined_at(text, occurrence, filename) {
        info.push(location);
    }

    let (markdown, offset, len) = assemble(name, &info, occurrence);

    HoverResult {
        markdown,
        offset,
        len,
    }
}

fn variant_parent_before(text: &str, offset: usize, len: usize) -> Option<(String, String)> {
    if offset == 0 || text.as_bytes().get(offset - 1) != Some(&b'.') {
        return None;
    }

    let mut start = offset - 1;

    while start > 0 && {
        let byte = text.as_bytes()[start - 1];
        byte.is_ascii_alphanumeric() || byte == b'_'
    } {
        start -= 1;
    }

    if start == offset - 1 {
        return None;
    }

    let variant = text.get(offset..offset + len)?.to_string();

    Some((text[start..offset - 1].to_string(), variant))
}

fn variant_hover(
    text: &str,
    analysis: &Analysis,
    name: &str,
    occurrence: &Occurrence,
    filename: &str,
) -> HoverResult {
    let mut info = vec!["enum member".to_string()];

    let owner = occurrence
        .def
        .and_then(|def| analysis.member_owner.get(&def).copied())
        .and_then(|owner| owner_name(text, analysis, owner).map(str::to_string))
        .or_else(|| {
            variant_parent_before(text, occurrence.offset, occurrence.len).map(|(parent, _)| parent)
        });

    let payload = owner.as_deref().and_then(|parent| {
        analysis.enum_payloads.get(parent).and_then(|variants| {
            variants
                .iter()
                .find_map(|(variant, payload)| (variant == name).then(|| payload.clone()).flatten())
        })
    });

    let code = match (owner, payload) {
        (_, Some(ty)) => format!("{name}: {ty}"),
        (Some(owner), None) => format!("{owner}.{name}"),
        (None, None) => name.to_string(),
    };

    if let Some(location) = defined_at(text, occurrence, filename) {
        info.push(location);
    }

    let (markdown, offset, len) = assemble(&code, &info, occurrence);

    HoverResult {
        markdown,
        offset,
        len,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::completion::ResolvePaths;

    const SAMPLE: &str = "const LIMIT: i32 = 10;\nlet counter: i32 = 0;\n\nstruct Point {\n    pub x: i32,\n    secret: i32,\n\n    pub fn make(x: i32, s: i32) Point {\n        return Point { .x = x, .secret = s };\n    }\n\n    pub fn get(*self) i32 {\n        return self.x;\n    }\n}\n\nenum Color {\n    red,\n    green,\n    num: i32,\n}\n\nfn add(a: i32, b: i32) i32 {\n    return a + b;\n}\n\nfn max[T](a: T, b: T) T {\n    return a;\n}\n\nfn main() {\n    let p = Point.make(1, 2);\n    let g = p.get();\n    let m = max(1, 2);\n    let c = Color.red;\n    let n = Color.num(7);\n    @println(\"{} {} {} {} {} {} {}\", p.x, g, m, @enumTag(c), @enumTag(n), LIMIT, counter);\n}\n";

    fn hovering(text: &str, marker: &str) -> String {
        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-hover-probe.zn".parse().unwrap();
        let output = crate::diagnostics::check(&uri, text, &[]);
        assert!(
            output.diagnostics.is_empty(),
            "sample must check cleanly: {:?}",
            output.diagnostics
        );

        let offset = text.find(marker).unwrap() + marker.len();
        let result = hover_markdown(
            text,
            &output.analysis,
            offset - marker.len(),
            ResolvePaths::default(),
            "hover-probe.zn",
        )
        .unwrap();

        result.markdown
    }

    #[test]
    fn local_shows_let_and_caps() {
        let markdown = hovering(SAMPLE, "p = Point.make");

        assert!(
            markdown.contains("```zeen\nlet p: Point\n```"),
            "{markdown}"
        );
        assert!(markdown.contains("Move-only, needs Drop"), "{markdown}");
    }

    #[test]
    fn constant_shows_const() {
        let markdown = hovering(SAMPLE, "LIMIT, counter");

        assert!(
            markdown.contains("```zeen\nconst LIMIT: i32\n```"),
            "{markdown}"
        );
    }

    #[test]
    fn global_mut_shows_let() {
        let markdown = hovering(SAMPLE, "counter)");

        assert!(
            markdown.contains("```zeen\nlet counter: i32\n```"),
            "{markdown}"
        );
    }

    #[test]
    fn param_shows_bare_name() {
        let markdown = hovering(SAMPLE, "a: i32, b: i32");

        assert!(markdown.contains("```zeen\na: i32\n```"), "{markdown}");
    }

    #[test]
    fn method_decl_shows_signature() {
        let markdown = hovering(SAMPLE, "get(*self)");

        assert!(markdown.contains("fn get(*self) i32"), "{markdown}");
        assert!(markdown.contains("instance method"), "{markdown}");
    }

    #[test]
    fn static_decl_shows_signature() {
        let markdown = hovering(SAMPLE, "make(x: i32");

        assert!(
            markdown.contains("fn make(x: i32, s: i32) Point"),
            "{markdown}"
        );
        assert!(markdown.contains("static method"), "{markdown}");
    }

    #[test]
    fn method_call_shows_instance() {
        let markdown = hovering(SAMPLE, "get();");

        assert!(markdown.contains("fn get(*self) i32"), "{markdown}");
        assert!(markdown.contains("instance method"), "{markdown}");
    }

    #[test]
    fn static_call_shows_static() {
        let markdown = hovering(SAMPLE, "make(1, 2)");

        assert!(
            markdown.contains("fn make(x: i32, s: i32) Point"),
            "{markdown}"
        );
        assert!(markdown.contains("static method"), "{markdown}");
    }

    #[test]
    fn free_call_shows_function() {
        let markdown = hovering(SAMPLE, "max(1, 2)");

        assert!(markdown.contains("function"), "{markdown}");
        assert!(!markdown.contains("method"), "{markdown}");
    }

    #[test]
    fn generic_fn_shows_params() {
        let markdown = hovering(SAMPLE, "max[T]");

        assert!(markdown.contains("fn max[T](a: T, b: T) T"), "{markdown}");
    }

    #[test]
    fn field_access_shows_pub() {
        let markdown = hovering(SAMPLE, "x, g, m");

        assert!(markdown.contains("```zeen\npub x: i32\n```"), "{markdown}");
    }

    #[test]
    fn variant_shows_owner() {
        let markdown = hovering(SAMPLE, "red;");

        assert!(markdown.contains("Color.red"), "{markdown}");
        assert!(markdown.contains("enum member"), "{markdown}");
    }

    #[test]
    fn field_decl_shows_type() {
        let markdown = hovering(SAMPLE, "x: i32,");

        assert!(markdown.contains("```zeen\npub x: i32\n```"), "{markdown}");
        assert!(markdown.contains("struct field"), "{markdown}");
    }

    #[test]
    fn global_decl_shows_type() {
        let markdown = hovering(SAMPLE, "LIMIT: i32");

        assert!(
            markdown.contains("```zeen\nconst LIMIT: i32\n```"),
            "{markdown}"
        );
    }

    #[test]
    fn free_fn_decl_shows_named_params() {
        let markdown = hovering(SAMPLE, "add(");

        assert!(
            markdown.contains("```zeen\nfn add(a: i32, b: i32) i32\n```"),
            "{markdown}"
        );
    }

    #[test]
    fn payload_variant_shows_payload() {
        let markdown = hovering(SAMPLE, "num(7)");

        assert!(markdown.contains("num: i32"), "{markdown}");
    }

    const BUFFER_SAMPLE: &str = "struct Buffer {\n    pub data: i32,\n\n    pub fn trim_end(*self) i32 {\n        return self.data;\n    }\n}\n\nfn main() {\n    let buffer = Buffer { .data = 1 };\n    let t = buffer.trim_end();\n    @println(\"{}\", t);\n}\n";

    #[test]
    fn struct_name_is_not_a_function() {
        let markdown = hovering(BUFFER_SAMPLE, "Buffer { .data");

        assert!(!markdown.contains("function"), "{markdown}");
        assert!(!markdown.contains("fn Buffer"), "{markdown}");
    }

    #[test]
    fn variable_is_not_a_function() {
        let markdown = hovering(BUFFER_SAMPLE, "buffer.trim_end");

        assert!(markdown.contains("let buffer: Buffer"), "{markdown}");
        assert!(!markdown.contains("function"), "{markdown}");
    }

    #[test]
    fn imported_type_shows_module() {
        let dir =
            std::env::temp_dir().join(format!("zeen-lsp-hover-imports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(
            dir.join("sub").join("lib.zn"),
            "pub struct Helper {\n    pub x: i32,\n\n    pub fn make() Helper {\n        return Helper { .x = 1 };\n    }\n}\n",
        )
        .unwrap();

        let text = "use sub.lib;\n\nfn main() {\n    let h = Helper.make();\n    @println(\"{}\", h.x);\n}\n";
        let uri: tower_lsp_server::ls_types::Uri =
            format!("file://{}", dir.join("main.zn").display())
                .parse()
                .unwrap();
        let output = crate::diagnostics::check(&uri, text, &[]);
        assert!(
            output.diagnostics.is_empty(),
            "sample must check cleanly: {:?}",
            output.diagnostics
        );

        let paths = ResolvePaths {
            dir: Some(dir.as_path()),
            root: Some(dir.as_path()),
            std: None,
            packages: None,
        };

        let offset = text.find("Helper.make").unwrap();
        let markdown = hover_markdown(text, &output.analysis, offset, paths, "main.zn").unwrap();
        assert!(
            markdown.markdown.contains("defined in `"),
            "{:?}",
            markdown.markdown
        );

        let offset = text.find("make();").unwrap();
        let markdown = hover_markdown(text, &output.analysis, offset, paths, "main.zn").unwrap();
        assert!(
            markdown.markdown.contains("static method"),
            "{:?}",
            markdown.markdown
        );
        assert!(
            markdown.markdown.contains("fn make(...)"),
            "{:?}",
            markdown.markdown
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
