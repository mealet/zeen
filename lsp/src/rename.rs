use std::collections::{HashMap, HashSet};

use tower_lsp_server::ls_types::{Range, TextEdit, Uri};

use crate::analysis::{Analysis, MemberKind};
use crate::position;

pub struct RenameDoc<'a> {
    pub uri: Uri,
    pub text: &'a str,
    pub analysis: &'a Analysis,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn target_uri(doc_uri: &Uri, target_file: Option<&str>) -> Option<Uri> {
    use std::str::FromStr;

    match target_file {
        None => Some(doc_uri.clone()),
        Some(name) => {
            let path = std::path::PathBuf::from(name);

            if path.is_absolute() {
                return Uri::from_str(&format!("file://{}", path.display())).ok();
            }

            let base = crate::diagnostics::file_uri_to_path(doc_uri)?;
            let parent = base.parent()?;

            Uri::from_str(&format!("file://{}", parent.join(name).display())).ok()
        }
    }
}

fn decl_span(docs: &[RenameDoc<'_>], def: zeen_resolve::DefId) -> Option<(usize, (usize, usize))> {
    for (index, doc) in docs.iter().enumerate() {
        let Some((offset, len, file)) = doc.analysis.def_spans.get(&def) else {
            continue;
        };

        match file {
            None => return Some((index, (*offset, *len))),
            Some(name) => {
                let uri = target_uri(&doc.uri, Some(name))?;

                if let Some(target) = docs.iter().position(|doc| doc.uri == uri) {
                    return Some((target, (*offset, *len)));
                }
            }
        }
    }

    None
}

fn field_owner(analysis: &Analysis, def: zeen_resolve::DefId) -> Option<zeen_resolve::DefId> {
    let is_field = analysis
        .members
        .values()
        .flatten()
        .any(|member| member.def == def && member.kind == MemberKind::Field);

    if !is_field {
        return None;
    }

    analysis.member_owner.get(&def).copied()
}

fn collect_spans(
    docs: &[RenameDoc<'_>],
    old: &str,
    def: zeen_resolve::DefId,
) -> Vec<(usize, (usize, usize))> {
    let mut spans = Vec::new();
    let mut seen: HashSet<(usize, usize, usize)> = HashSet::new();

    let mut push =
        |index: usize, span: (usize, usize), spans: &mut Vec<(usize, (usize, usize))>| {
            if docs[index].text.get(span.0..span.0 + span.1) != Some(old) {
                return;
            }

            if seen.insert((index, span.0, span.1)) {
                spans.push((index, span));
            }
        };

    let anchor = decl_span(docs, def).map(|(doc, span)| (docs[doc].uri.clone(), span));

    if let Some((doc, span)) = decl_span(docs, def) {
        push(doc, span, &mut spans);
    }

    for (index, doc) in docs.iter().enumerate() {
        for occurrence in &doc.analysis.occurrences {
            if occurrence.def != Some(def) {
                continue;
            }

            let Some(target_offset) = occurrence.target_offset else {
                continue;
            };

            let Some(target_len) = occurrence.target_len else {
                continue;
            };

            let matches_decl = anchor.as_ref().is_some_and(|(uri, span)| {
                target_uri(&doc.uri, occurrence.target_file.as_deref()) == Some(uri.clone())
                    && target_offset == span.0
                    && target_len == span.1
            });

            if matches_decl {
                push(index, (occurrence.offset, occurrence.len), &mut spans);
            }
        }

        for (span, target) in &doc.analysis.method_calls {
            if *target != def {
                continue;
            }

            let field = doc
                .analysis
                .raw_calls
                .iter()
                .find_map(|raw| (raw.callee == *span).then_some(raw.field).flatten());

            match field {
                Some(field) => push(index, field, &mut spans),
                None => push(index, *span, &mut spans),
            }
        }

        for (span, target) in &doc.analysis.field_targets {
            if target.field_def != def {
                continue;
            }

            push(index, *span, &mut spans);
        }

        if let Some(owner) = field_owner(doc.analysis, def) {
            for init in &doc.analysis.raw_fields {
                if init.struct_def != owner {
                    continue;
                }

                let Some(body) = docs[index].text.get(init.offset..init.offset + init.len) else {
                    continue;
                };

                let Some(dot) = body.find('.') else {
                    continue;
                };

                push(index, (init.offset + dot + 1, old.len()), &mut spans);
            }
        }
    }

    spans
}

pub fn rename_edits(
    docs: &[RenameDoc<'_>],
    uri: &Uri,
    offset: usize,
    new_name: &str,
) -> HashMap<Uri, Vec<TextEdit>> {
    let mut out: HashMap<Uri, Vec<TextEdit>> = HashMap::new();

    if !valid_name(new_name) {
        return out;
    }

    let Some(home) = docs.iter().position(|doc| doc.uri == *uri) else {
        return out;
    };

    let Some((old, def)) = rename_target(docs, home, offset) else {
        return out;
    };

    if old == new_name {
        return out;
    }

    for (index, span) in collect_spans(docs, old, def) {
        out.entry(docs[index].uri.clone())
            .or_default()
            .push(TextEdit {
                range: position::span_to_range(docs[index].text, span.0, span.1),
                new_text: new_name.to_string(),
            });
    }

    out.values_mut().for_each(|edits| {
        edits.sort_by_key(|edit| (edit.range.start.line, edit.range.start.character));
    });

    out
}

fn rename_target<'a>(
    docs: &'a [RenameDoc<'a>],
    home: usize,
    offset: usize,
) -> Option<(&'a str, zeen_resolve::DefId)> {
    let analysis = docs[home].analysis;
    let occurrence = analysis.at(offset)?;

    let old = docs[home]
        .text
        .get(occurrence.offset..occurrence.offset + occurrence.len)
        .filter(|name| !name.is_empty())?;

    let key = (occurrence.offset, occurrence.len);

    match crate::hover::method_at(analysis, key) {
        Some(fn_def) => Some((old, fn_def)),
        None => analysis
            .field_targets
            .get(&key)
            .map(|target| target.field_def)
            .or(occurrence.def)
            .map(|def| (old, def)),
    }
}

pub fn count_references(docs: &[RenameDoc<'_>], uri: &Uri, offset: usize) -> usize {
    reference_spans(docs, uri, offset).len()
}

pub fn reference_spans(docs: &[RenameDoc<'_>], uri: &Uri, offset: usize) -> Vec<(Uri, Range)> {
    let Some(home) = docs.iter().position(|doc| doc.uri == *uri) else {
        return Vec::new();
    };

    let Some((old, def)) = rename_target(docs, home, offset) else {
        return Vec::new();
    };

    let decl = decl_span(docs, def);

    collect_spans(docs, old, def)
        .into_iter()
        .filter(|(index, span)| {
            decl.is_some_and(|(decl_doc, decl_span)| decl_doc != *index || *span != decl_span)
        })
        .map(|(index, span)| {
            (
                docs[index].uri.clone(),
                position::span_to_range(docs[index].text, span.0, span.1),
            )
        })
        .collect()
}

pub fn decl_position(docs: &[RenameDoc<'_>], uri: &Uri, offset: usize) -> Option<(Uri, Range)> {
    let home = docs.iter().position(|doc| doc.uri == *uri)?;
    let (_, def) = rename_target(docs, home, offset)?;
    let (doc, span) = decl_span(docs, def)?;

    Some((
        docs[doc].uri.clone(),
        position::span_to_range(docs[doc].text, span.0, span.1),
    ))
}

pub fn rename_range(docs: &[RenameDoc<'_>], uri: &Uri, offset: usize) -> Option<Range> {
    let home = docs.iter().position(|doc| doc.uri == *uri)?;
    let analysis = docs[home].analysis;
    let occurrence = analysis.at(offset)?;
    let _ = rename_target(docs, home, offset)?;

    Some(position::span_to_range(
        docs[home].text,
        occurrence.offset,
        occurrence.len,
    ))
}

pub fn highlight_spans(docs: &[RenameDoc<'_>], uri: &Uri, offset: usize) -> Vec<(Range, bool)> {
    let Some(home) = docs.iter().position(|doc| doc.uri == *uri) else {
        return Vec::new();
    };

    let Some((old, def)) = rename_target(docs, home, offset) else {
        return Vec::new();
    };

    let write = decl_span(docs, def);

    collect_spans(docs, old, def)
        .into_iter()
        .filter(|(index, _)| *index == home)
        .map(|(index, span)| {
            let is_write =
                write.is_some_and(|(decl_doc, decl_span)| decl_doc == index && decl_span == span);

            (
                position::span_to_range(docs[index].text, span.0, span.1),
                is_write,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        uris: Vec<Uri>,
        texts: Vec<String>,
        analyses: Vec<Analysis>,
    }

    impl Fixture {
        fn check(texts: &[(&str, &str)]) -> Self {
            let mut uris = Vec::new();
            let mut owned = Vec::new();
            let mut analyses = Vec::new();

            for (name, text) in texts {
                let uri: Uri = format!("file:///tmp/zeen-lsp-rename-{name}.zn")
                    .parse()
                    .unwrap();
                let output = crate::diagnostics::check(&uri, text, &[]);
                assert!(
                    output.diagnostics.is_empty(),
                    "sample must check cleanly: {:?}",
                    output.diagnostics
                );

                uris.push(uri);
                owned.push(text.to_string());
                analyses.push(output.analysis);
            }

            let _ = owned;

            Self {
                uris,
                texts: texts.iter().map(|(_, text)| text.to_string()).collect(),
                analyses,
            }
        }

        fn docs(&self) -> Vec<RenameDoc<'_>> {
            self.uris
                .iter()
                .zip(self.texts.iter().zip(self.analyses.iter()))
                .map(|(uri, (text, analysis))| RenameDoc {
                    uri: uri.clone(),
                    text,
                    analysis,
                })
                .collect()
        }

        fn offset(&self, doc: usize, marker: &str) -> usize {
            self.texts[doc].find(marker).unwrap()
        }
    }

    fn applied(text: &str, edits: &[TextEdit]) -> String {
        let mut out = text.to_string();

        for edit in edits.iter().rev() {
            let start = position::position_to_offset(&out, edit.range.start);
            let end = position::position_to_offset(&out, edit.range.end);
            out.replace_range(start..end, &edit.new_text);
        }

        out
    }

    const FN_SAMPLE: &str = "fn add(a: i32, b: i32) i32 {\n    return a + b;\n}\n\nfn main() {\n    let x = add(1, 2);\n    @println(\"{}\", x);\n}\n";

    #[test]
    fn renames_function_everywhere() {
        let fixture = Fixture::check(&[("main", FN_SAMPLE)]);
        let docs = fixture.docs();
        let offset = fixture.offset(0, "add(1, 2)");
        let edits = rename_edits(&docs, &fixture.uris[0], offset, "sum");

        assert_eq!(edits.len(), 1);

        let result = applied(&fixture.texts[0], &edits[&fixture.uris[0]]);

        assert!(result.contains("fn sum(a: i32, b: i32) i32"));
        assert!(!result.contains("add(1, 2)"));
    }

    const METHOD_SAMPLE: &str = "struct Box {\n    pub v: i32,\n\n    pub fn get(*self) i32 {\n        return self.v;\n    }\n}\n\nfn main() {\n    let b = Box { .v = 1 };\n    @println(\"{}\", b.get());\n}\n";

    #[test]
    fn renames_method_without_touching_object() {
        let fixture = Fixture::check(&[("main", METHOD_SAMPLE)]);
        let docs = fixture.docs();
        let offset = fixture.offset(0, "b.get()") + "b.".len();
        let edits = rename_edits(&docs, &fixture.uris[0], offset, "fetch");

        let result = applied(&fixture.texts[0], &edits[&fixture.uris[0]]);

        assert!(result.contains("fn fetch(*self) i32"));
        assert!(result.contains("b.fetch()"));
        assert!(!result.contains("get"));
    }

    const FIELD_SAMPLE: &str = "struct Box {\n    pub v: i32,\n}\n\nfn main() {\n    let b = Box { .v = 2 };\n    @println(\"{}\", b.v);\n}\n";

    #[test]
    fn renames_field_decl_access_and_init() {
        let fixture = Fixture::check(&[("main", FIELD_SAMPLE)]);
        let docs = fixture.docs();
        let offset = fixture.offset(0, "b.v);") + "b.".len();
        let edits = rename_edits(&docs, &fixture.uris[0], offset, "value");

        let result = applied(&fixture.texts[0], &edits[&fixture.uris[0]]);

        assert!(result.contains("pub value: i32"));
        assert!(result.contains(".value = 2"));
        assert!(result.contains("b.value"));
    }

    #[test]
    fn rejects_bad_names() {
        let fixture = Fixture::check(&[("main", FN_SAMPLE)]);
        let docs = fixture.docs();
        let offset = fixture.offset(0, "add(1, 2)");

        assert!(rename_edits(&docs, &fixture.uris[0], offset, "").is_empty());
        assert!(rename_edits(&docs, &fixture.uris[0], offset, "has space").is_empty());
        assert!(rename_edits(&docs, &fixture.uris[0], offset, "add").is_empty());
    }

    #[test]
    fn counts_references() {
        let fixture = Fixture::check(&[("main", FN_SAMPLE)]);
        let docs = fixture.docs();

        assert_eq!(
            count_references(&docs, &fixture.uris[0], fixture.offset(0, "add(1, 2)")),
            1
        );
        assert_eq!(
            count_references(&docs, &fixture.uris[0], fixture.offset(0, "add(a: i32")),
            1
        );
    }
}
