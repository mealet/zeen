use tower_lsp_server::ls_types::{CodeLens, Command, Uri};

use crate::analysis::{Analysis, Role};
use crate::position;
use crate::rename::RenameDoc;

pub const RUN_COMMAND: &str = "zeen.runMain";
pub const REFERENCES_COMMAND: &str = "zeen.showReferences";
pub const TEST_COMMAND: &str = "zeen.runTest";

pub fn runnable(zora_root: Option<&std::path::Path>, open_text: &str) -> bool {
    match zora_root {
        Some(root) => {
            let Ok(manifest) = std::fs::read_to_string(root.join(crate::zora::MANIFEST)) else {
                return false;
            };

            matches!(
                crate::zora::entry_kind(&manifest),
                Some(crate::zora::EntryKind::Main)
            )
        }
        None => crate::diagnostics::contains_main(open_text),
    }
}

pub fn run_lens(
    text: &str,
    analysis: &Analysis,
    uri: &Uri,
    entry_is_open: bool,
    runnable: bool,
) -> Option<CodeLens> {
    if !entry_is_open || !runnable {
        return None;
    }

    let occurrence = analysis.occurrences.iter().find(|occurrence| {
        occurrence.role == Role::Function
            && occurrence.target_offset.is_none()
            && text.get(occurrence.offset..occurrence.offset + occurrence.len) == Some("main")
    })?;

    Some(CodeLens {
        range: position::span_to_range(text, occurrence.offset, occurrence.len),
        command: Some(Command {
            title: "▶ run".to_string(),
            command: RUN_COMMAND.to_string(),
            arguments: Some(vec![serde_json::Value::String(uri.as_str().to_string())]),
        }),
        data: None,
    })
}

pub fn reference_lenses(
    text: &str,
    analysis: &Analysis,
    docs: &[RenameDoc<'_>],
    uri: &Uri,
) -> Vec<CodeLens> {
    let mut lenses = Vec::new();

    for occurrence in &analysis.occurrences {
        if occurrence.role != Role::Function || occurrence.target_offset.is_some() {
            continue;
        }

        let count = crate::rename::count_references(docs, uri, occurrence.offset);
        let title = if count == 1 {
            "1 reference".to_string()
        } else {
            format!("{count} references")
        };

        lenses.push(CodeLens {
            range: position::span_to_range(text, occurrence.offset, occurrence.len),
            command: Some(Command {
                title,
                command: REFERENCES_COMMAND.to_string(),
                arguments: Some(vec![
                    serde_json::Value::String(uri.as_str().to_string()),
                    serde_json::Value::Number(serde_json::Number::from(occurrence.offset)),
                ]),
            }),
            data: None,
        });
    }

    lenses
}

pub fn test_lenses(text: &str, uri: &Uri) -> Vec<CodeLens> {
    let mut lenses = Vec::new();
    let mut marked = false;
    let mut offset = 0;

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        let lowered = trimmed.to_lowercase();

        if lowered == "/// @test" {
            marked = true;
            offset += line.len();
            continue;
        }

        if trimmed.is_empty() || trimmed.starts_with("//") {
            offset += line.len();
            continue;
        }

        if marked {
            if let Some((name_offset, name)) = pub_fn_name(line) {
                let start = offset + name_offset;

                lenses.push(CodeLens {
                    range: position::span_to_range(text, start, name.len()),
                    command: Some(Command {
                        title: "▶ test".to_string(),
                        command: TEST_COMMAND.to_string(),
                        arguments: Some(vec![
                            serde_json::Value::String(uri.as_str().to_string()),
                            serde_json::Value::String(name.to_string()),
                        ]),
                    }),
                    data: None,
                });
            }

            marked = false;
        }

        offset += line.len();
    }

    lenses
}

fn pub_fn_name(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    let rest = trimmed.strip_prefix("pub fn")?;
    let after_keyword = rest.trim_start();
    let lead = rest.len() - after_keyword.len();
    let name = after_keyword
        .split(|c: char| c == '(' || c.is_whitespace())
        .next()?;

    if name.is_empty() {
        return None;
    }

    Some((indent + "pub fn".len() + lead, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rename::RenameDoc;

    const SAMPLE: &str = "fn add(a: i32, b: i32) i32 {\n    return a + b;\n}\n\nfn main() {\n    let x = add(1, 2);\n    @println(\"{}\", x);\n}\n";

    const LIB_SAMPLE: &str = "pub struct Box {\n    pub v: i32,\n}\n";

    fn sample_analysis(text: &str) -> crate::analysis::Analysis {
        let uri: Uri = "file:///tmp/zeen-lsp-codelens-probe.zn".parse().unwrap();
        let output = crate::diagnostics::check(&uri, text, &[]);
        assert!(output.diagnostics.is_empty());
        output.analysis
    }

    #[test]
    fn run_lens_marks_entry_main() {
        let analysis = sample_analysis(SAMPLE);
        let uri: Uri = "file:///tmp/zeen-lsp-codelens-probe.zn".parse().unwrap();

        let lens = run_lens(SAMPLE, &analysis, &uri, true, true).unwrap();
        assert_eq!(lens.command.as_ref().unwrap().title, "▶ run");
        assert_eq!(lens.command.as_ref().unwrap().command, "zeen.runMain");

        assert!(run_lens(SAMPLE, &analysis, &uri, false, true).is_none());
        assert!(run_lens(SAMPLE, &analysis, &uri, true, false).is_none());
    }

    #[test]
    fn lib_without_main_has_no_run_lens() {
        let analysis = sample_analysis(LIB_SAMPLE);
        let uri: Uri = "file:///tmp/zeen-lsp-codelens-probe.zn".parse().unwrap();

        assert!(run_lens(LIB_SAMPLE, &analysis, &uri, true, true).is_none());
    }

    #[test]
    fn references_lens_counts_uses() {
        let analysis = sample_analysis(SAMPLE);
        let uri: Uri = "file:///tmp/zeen-lsp-codelens-probe.zn".parse().unwrap();
        let docs = [RenameDoc {
            uri: uri.clone(),
            text: SAMPLE,
            analysis: &analysis,
        }];

        let add_at = SAMPLE.find("fn add(").unwrap() + "fn ".len();
        let lenses = reference_lenses(SAMPLE, &analysis, &docs, &uri);
        let add = lenses
            .iter()
            .find(|lens| crate::position::position_to_offset(SAMPLE, lens.range.start) == add_at)
            .unwrap();

        assert_eq!(add.command.as_ref().unwrap().title, "1 reference");
    }

    #[test]
    fn runnable_detects_bin_projects() {
        assert!(runnable(None, "fn main() {}"));
        assert!(!runnable(None, "fn helper() {}"));
    }

    #[test]
    fn test_lenses_mark_marked_public_functions() {
        let text = "/// @test\npub fn adds() {\n}\n\n/// @test\n/// @should-fail\npub fn panics() {\n}\n\npub fn helper() {\n}\n";
        let uri: Uri = "file:///tmp/zeen-lsp-codelens-probe/tests/math.zn"
            .parse()
            .unwrap();

        let lenses = test_lenses(text, &uri);
        assert_eq!(lenses.len(), 2);

        let names: Vec<usize> = lenses
            .iter()
            .map(|lens| crate::position::position_to_offset(text, lens.range.start))
            .collect();
        assert_eq!(names[0], text.find("adds").unwrap());
        assert_eq!(names[1], text.find("panics").unwrap());

        for lens in &lenses {
            let command = lens.command.as_ref().unwrap();
            assert_eq!(command.title, "▶ test");
            assert_eq!(command.command, "zeen.runTest");
        }
    }

    #[test]
    fn test_lenses_skip_untagged_functions() {
        let text = "pub fn helper() {\n}\n";
        let uri: Uri = "file:///tmp/zeen-lsp-codelens-probe/tests/math.zn"
            .parse()
            .unwrap();

        assert!(test_lenses(text, &uri).is_empty());
    }
}
