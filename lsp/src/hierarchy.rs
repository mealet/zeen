use std::collections::HashMap;

use tower_lsp_server::ls_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, Range, SymbolKind,
    TypeHierarchyItem, Uri,
};

use crate::analysis::{Analysis, Role};
use crate::position;

pub struct HierarchyDoc<'a> {
    pub uri: Uri,
    pub text: &'a str,
    pub analysis: &'a Analysis,
}

fn is_callable(role: Role) -> bool {
    matches!(role, Role::Function | Role::Method)
}

fn encode(uri: &Uri, offset: usize) -> serde_json::Value {
    serde_json::json!({ "uri": uri.as_str(), "offset": offset })
}

fn decode(data: &serde_json::Value) -> Option<(Uri, usize)> {
    let uri: Uri = data.get("uri")?.as_str()?.parse().ok()?;
    let offset = data.get("offset")?.as_u64()? as usize;

    Some((uri, offset))
}

fn decl_doc(docs: &[HierarchyDoc<'_>], def: zeen_resolve::DefId) -> Option<usize> {
    docs.iter()
        .position(|doc| doc.analysis.full_spans.contains_key(&def))
}

fn name_occurrence(
    analysis: &Analysis,
    def: zeen_resolve::DefId,
) -> Option<&crate::analysis::Occurrence> {
    analysis
        .occurrences
        .iter()
        .find(|occurrence| occurrence.def == Some(def) && occurrence.target_offset.is_none())
}

fn span_range(text: &str, span: (usize, usize)) -> Range {
    position::span_to_range(text, span.0, span.1)
}

fn def_at(docs: &[HierarchyDoc<'_>], uri: &Uri, offset: usize) -> Option<zeen_resolve::DefId> {
    let home = docs.iter().position(|doc| doc.uri == *uri)?;
    let analysis = docs[home].analysis;
    let occurrence = analysis.at(offset)?;

    analysis
        .method_calls
        .get(&(occurrence.offset, occurrence.len))
        .copied()
        .or(occurrence.def)
}

fn call_item(docs: &[HierarchyDoc<'_>], def: zeen_resolve::DefId) -> Option<CallHierarchyItem> {
    let index = decl_doc(docs, def)?;
    let doc = &docs[index];
    let occurrence = name_occurrence(doc.analysis, def)?;
    let (start, end) = doc.analysis.full_spans.get(&def).copied()?;
    let name = doc
        .text
        .get(occurrence.offset..occurrence.offset + occurrence.len)?;

    Some(CallHierarchyItem {
        name: name.to_string(),
        kind: if occurrence.role == Role::Method {
            SymbolKind::METHOD
        } else {
            SymbolKind::FUNCTION
        },
        tags: None,
        detail: doc.analysis.fn_source_sig.get(&def).cloned(),
        uri: doc.uri.clone(),
        range: position::span_to_range(doc.text, start, end - start),
        selection_range: position::span_to_range(doc.text, occurrence.offset, occurrence.len),
        data: Some(encode(&doc.uri, occurrence.offset)),
    })
}

pub fn prepare(docs: &[HierarchyDoc<'_>], uri: &Uri, offset: usize) -> Option<CallHierarchyItem> {
    let home = docs.iter().position(|doc| doc.uri == *uri)?;
    let analysis = docs[home].analysis;
    let occurrence = analysis.at(offset)?;
    let key = (occurrence.offset, occurrence.len);
    let def = def_at(docs, uri, offset)?;

    if !is_callable(occurrence.role) && !analysis.method_calls.contains_key(&key) {
        return None;
    }

    call_item(docs, def)
}

pub fn outgoing(
    docs: &[HierarchyDoc<'_>],
    item: &CallHierarchyItem,
) -> Vec<CallHierarchyOutgoingCall> {
    let Some((uri, offset)) = item.data.as_ref().and_then(decode) else {
        return Vec::new();
    };

    let Some(home) = docs.iter().position(|doc| doc.uri == uri) else {
        return Vec::new();
    };

    let doc = &docs[home];
    let analysis = doc.analysis;

    let Some(occurrence) = analysis.at(offset) else {
        return Vec::new();
    };

    let Some(def) = occurrence.def else {
        return Vec::new();
    };

    let Some((start, end)) = analysis.full_spans.get(&def).copied() else {
        return Vec::new();
    };

    let mut grouped: HashMap<zeen_resolve::DefId, Vec<Range>> = HashMap::new();

    for (key, callee) in &analysis.method_calls {
        if *callee == def || key.0 < start || key.0 + key.1 > end {
            continue;
        }

        grouped
            .entry(*callee)
            .or_default()
            .push(span_range(doc.text, *key));
    }

    let mut out = Vec::new();

    for (callee, from_ranges) in grouped {
        if let Some(to) = call_item(docs, callee) {
            out.push(CallHierarchyOutgoingCall { to, from_ranges });
        }
    }

    out.sort_by(|a, b| a.to.name.cmp(&b.to.name));
    out
}

pub fn incoming(
    docs: &[HierarchyDoc<'_>],
    item: &CallHierarchyItem,
) -> Vec<CallHierarchyIncomingCall> {
    let Some((uri, offset)) = item.data.as_ref().and_then(decode) else {
        return Vec::new();
    };

    let Some(home) = docs.iter().position(|doc| doc.uri == uri) else {
        return Vec::new();
    };

    let Some(occurrence) = docs[home].analysis.at(offset) else {
        return Vec::new();
    };

    let Some(def) = occurrence.def else {
        return Vec::new();
    };

    let mut grouped: HashMap<zeen_resolve::DefId, Vec<Range>> = HashMap::new();

    for doc in docs {
        for (key, callee) in &doc.analysis.method_calls {
            if *callee != def {
                continue;
            }

            let Some(caller) = enclosing(doc.analysis, key.0) else {
                continue;
            };

            grouped
                .entry(caller)
                .or_default()
                .push(span_range(doc.text, *key));
        }
    }

    let mut out = Vec::new();

    for (caller, from_ranges) in grouped {
        if let Some(from) = call_item(docs, caller) {
            out.push(CallHierarchyIncomingCall { from, from_ranges });
        }
    }

    out.sort_by(|a, b| a.from.name.cmp(&b.from.name));
    out
}

fn enclosing(analysis: &Analysis, offset: usize) -> Option<zeen_resolve::DefId> {
    analysis
        .full_spans
        .iter()
        .filter(|(_, (start, end))| *start <= offset && offset < *end)
        .min_by_key(|(_, (start, end))| end - start)
        .map(|(def, _)| *def)
}

pub fn prepare_type(
    docs: &[HierarchyDoc<'_>],
    uri: &Uri,
    offset: usize,
) -> Option<TypeHierarchyItem> {
    let index = docs.iter().position(|doc| doc.uri == *uri)?;
    let doc = &docs[index];
    let occurrence = doc.analysis.at(offset)?;
    let def = occurrence.type_def.or(occurrence.def)?;

    type_item(docs, def)
}

fn type_item(docs: &[HierarchyDoc<'_>], def: zeen_resolve::DefId) -> Option<TypeHierarchyItem> {
    let index = decl_doc(docs, def)?;
    let doc = &docs[index];
    let occurrence = name_occurrence(doc.analysis, def)?;
    let (start, end) = doc.analysis.full_spans.get(&def).copied()?;
    let name = doc
        .text
        .get(occurrence.offset..occurrence.offset + occurrence.len)?;

    Some(TypeHierarchyItem {
        name: name.to_string(),
        kind: type_kind(doc.analysis, def),
        tags: None,
        detail: None,
        uri: doc.uri.clone(),
        range: position::span_to_range(doc.text, start, end - start),
        selection_range: position::span_to_range(doc.text, occurrence.offset, occurrence.len),
        data: Some(encode(&doc.uri, occurrence.offset)),
    })
}

fn type_kind(analysis: &Analysis, def: zeen_resolve::DefId) -> SymbolKind {
    match analysis.type_kinds.get(&def) {
        Some(crate::analysis::TypeKind::Enum) => SymbolKind::ENUM,
        Some(crate::analysis::TypeKind::Interface) => SymbolKind::INTERFACE,
        _ => SymbolKind::STRUCT,
    }
}

pub fn supertypes(docs: &[HierarchyDoc<'_>], item: &TypeHierarchyItem) -> Vec<TypeHierarchyItem> {
    let Some((uri, offset)) = item.data.as_ref().and_then(decode) else {
        return Vec::new();
    };

    let Some(index) = docs.iter().position(|doc| doc.uri == uri) else {
        return Vec::new();
    };

    let Some(occurrence) = docs[index].analysis.at(offset) else {
        return Vec::new();
    };

    let Some(def) = occurrence.type_def.or(occurrence.def) else {
        return Vec::new();
    };

    let mut defs: Vec<zeen_resolve::DefId> = docs
        .iter()
        .flat_map(|doc| doc.analysis.implements.iter())
        .filter(|(_, object)| *object == def)
        .map(|(interface, _)| *interface)
        .collect();

    defs.sort();
    defs.dedup();

    defs.into_iter()
        .filter_map(|def| type_item(docs, def))
        .collect()
}

pub fn subtypes(docs: &[HierarchyDoc<'_>], item: &TypeHierarchyItem) -> Vec<TypeHierarchyItem> {
    let Some((uri, offset)) = item.data.as_ref().and_then(decode) else {
        return Vec::new();
    };

    let Some(index) = docs.iter().position(|doc| doc.uri == uri) else {
        return Vec::new();
    };

    let Some(occurrence) = docs[index].analysis.at(offset) else {
        return Vec::new();
    };

    let Some(def) = occurrence.type_def.or(occurrence.def) else {
        return Vec::new();
    };

    let mut defs: Vec<zeen_resolve::DefId> = docs
        .iter()
        .flat_map(|doc| doc.analysis.implements.iter())
        .filter(|(interface, _)| *interface == def)
        .map(|(_, object)| *object)
        .collect();

    defs.sort();
    defs.dedup();

    defs.into_iter()
        .filter_map(|def| type_item(docs, def))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docs<'a>(texts: &'a [(&'a str, Uri, Analysis)]) -> Vec<HierarchyDoc<'a>> {
        texts
            .iter()
            .map(|(text, uri, analysis)| HierarchyDoc {
                uri: uri.clone(),
                text,
                analysis,
            })
            .collect()
    }

    fn analyze(uri: &Uri, text: &str) -> Analysis {
        let output = crate::diagnostics::check(uri, text, &[]);
        assert!(output.diagnostics.is_empty());
        output.analysis
    }

    const CALLERS: &str = "fn leaf() {}\n\nfn mid() {\n    leaf();\n    leaf();\n}\n\nfn top() {\n    mid();\n}\n\nfn main() {\n    top();\n}\n";

    #[test]
    fn outgoing_lists_callees() {
        let uri: Uri = "file:///tmp/zeen-lsp-hierarchy-callees.zn".parse().unwrap();
        let analysis = analyze(&uri, CALLERS);
        let store = [(CALLERS, uri.clone(), analysis)];
        let docs = docs(&store);

        let mid_at = CALLERS.find("fn mid").unwrap() + "fn ".len();
        let root = prepare(&docs, &uri, mid_at).unwrap();
        assert_eq!(root.name, "mid");

        let calls = outgoing(&docs, &root);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].to.name, "leaf");
        assert_eq!(calls[0].from_ranges.len(), 2);
    }

    #[test]
    fn incoming_lists_callers() {
        let uri: Uri = "file:///tmp/zeen-lsp-hierarchy-callers.zn".parse().unwrap();
        let analysis = analyze(&uri, CALLERS);
        let store = [(CALLERS, uri.clone(), analysis)];
        let docs = docs(&store);

        let leaf_at = CALLERS.find("fn leaf").unwrap() + "fn ".len();
        let root = prepare(&docs, &uri, leaf_at).unwrap();
        assert_eq!(root.name, "leaf");

        let calls = incoming(&docs, &root);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].from.name, "mid");
        assert_eq!(calls[0].from_ranges.len(), 2);
    }

    const SHAPES: &str = "interface Describe {\n    fn kind(*const self) i32;\n}\n\nstruct Point {\n    pub x: i32,\n}\n\nimplement Describe : Point {\n    fn kind(*const self) i32 {\n        return self.x;\n    }\n}\n";

    #[test]
    fn type_hierarchy_links_struct_and_interface() {
        let uri: Uri = "file:///tmp/zeen-lsp-hierarchy-shapes.zn".parse().unwrap();
        let analysis = analyze(&uri, SHAPES);
        let store = [(SHAPES, uri.clone(), analysis)];
        let docs = docs(&store);

        let point_at = SHAPES.find("struct Point").unwrap() + "struct ".len();
        let point = prepare_type(&docs, &uri, point_at).unwrap();
        assert_eq!(point.name, "Point");

        let parents = supertypes(&docs, &point);
        assert_eq!(parents.len(), 1);
        assert_eq!(parents[0].name, "Describe");
        assert_eq!(parents[0].kind, SymbolKind::INTERFACE);

        let children = subtypes(&docs, &parents[0]);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "Point");
    }
}
