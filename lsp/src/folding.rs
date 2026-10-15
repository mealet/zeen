use tower_lsp_server::ls_types::{FoldingRange, Position, Range, SelectionRange};

use crate::analysis::Analysis;
use crate::position;

pub fn folding_ranges(text: &str, analysis: &Analysis) -> Vec<FoldingRange> {
    let mut spans: Vec<(usize, usize)> = Vec::new();

    for scope in &analysis.scopes {
        if scope.end > scope.start {
            spans.push((scope.start, scope.end));
        }
    }

    for (offset, end) in analysis.full_spans.values() {
        if *end > *offset {
            spans.push((*offset, *end));
        }
    }

    spans.sort();
    spans.dedup();

    let mut out = Vec::new();

    for (start, end) in spans {
        let start_line = position::offset_to_position(text, start).line;
        let end_line = position::offset_to_position(text, end.min(text.len())).line;

        if end_line > start_line {
            out.push(FoldingRange {
                start_line,
                start_character: None,
                end_line,
                end_character: None,
                kind: None,
                collapsed_text: None,
            });
        }
    }

    out
}

pub fn selection_ranges(
    text: &str,
    analysis: &Analysis,
    positions: &[Position],
) -> Vec<SelectionRange> {
    positions
        .iter()
        .map(|position| {
            let offset = position::position_to_offset(text, *position);
            expand(text, analysis, offset)
        })
        .collect()
}

fn expand(text: &str, analysis: &Analysis, offset: usize) -> SelectionRange {
    let mut node = match analysis.at(offset) {
        Some(occurrence) => SelectionRange {
            range: position::span_to_range(text, occurrence.offset, occurrence.len),
            parent: None,
        },
        None => SelectionRange {
            range: position::span_to_range(text, offset, 0),
            parent: None,
        },
    };

    let mut containers: Vec<(usize, usize)> = analysis
        .scopes
        .iter()
        .filter(|scope| scope.start <= offset && offset <= scope.end)
        .map(|scope| (scope.start, scope.end))
        .collect();

    if analysis.at(offset).is_some() {
        for (span_offset, span_end) in analysis.full_spans.values() {
            if *span_offset <= offset && offset <= *span_end {
                containers.push((*span_offset, *span_end));
            }
        }
    }

    containers.sort();
    containers.dedup();

    for (start, end) in containers {
        node = SelectionRange {
            range: position::span_to_range(text, start, end - start),
            parent: Some(Box::new(node)),
        };
    }

    node
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "struct Point {\n    pub x: i32,\n}\n\nfn add(a: i32, b: i32) i32 {\n    return a + b;\n}\n\nfn main() {\n    let v = add(1, 2);\n    let w = Wrap { .v = 3 };\n    @println(\"{} {}\", v, w.get());\n}\n\nstruct Wrap {\n    pub v: i32,\n\n    pub fn get(*self) i32 {\n        return self.v;\n    }\n}\n";

    fn sample_analysis(text: &str) -> Analysis {
        let uri: tower_lsp_server::ls_types::Uri =
            "file:///tmp/zeen-lsp-folding-probe.zn".parse().unwrap();
        let output = crate::diagnostics::check(&uri, text, &[]);
        assert!(output.diagnostics.is_empty());
        output.analysis
    }

    #[test]
    fn folds_types_and_functions() {
        let analysis = sample_analysis(SAMPLE);
        let ranges = folding_ranges(SAMPLE, &analysis);
        let lines: Vec<(u32, u32)> = ranges
            .iter()
            .map(|range| (range.start_line, range.end_line))
            .collect();

        assert!(lines.contains(&(0, 2)));
        assert!(lines.contains(&(4, 6)));
    }

    #[test]
    fn selection_expands_outward() {
        let analysis = sample_analysis(SAMPLE);
        let offset = SAMPLE.find("return self.v;").unwrap() + "return ".len();
        let position = position::offset_to_position(SAMPLE, offset);
        let ranges = selection_ranges(SAMPLE, &analysis, &[position]);

        assert_eq!(ranges.len(), 1);

        let mut depth = 0;
        let mut current = &ranges[0];

        while let Some(parent) = current.parent.as_deref() {
            depth += 1;
            current = parent;
        }

        assert!(depth >= 2);
    }
}
