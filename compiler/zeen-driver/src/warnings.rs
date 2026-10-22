use std::sync::Arc;

use miette::NamedSource;

pub fn source_disables_warnings(source: &NamedSource<Arc<String>>) -> bool {
    find_marker(source.inner().as_bytes())
}

fn find_marker(data: &[u8]) -> bool {
    data.windows(MARKER.len()).any(|window| window == MARKER)
}

const MARKER: &[u8] = b"// @no-warnings";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_line_marker() {
        assert!(source_disables_warnings(&src_of("// @no-warnings")));
        assert!(source_disables_warnings(&src_of(
            "let x = 1;\n// @no-warnings\nlet y = 2;"
        )));
    }

    #[test]
    fn requires_comment_prefix() {
        assert!(!source_disables_warnings(&src_of("let x = 1;")));
        assert!(!source_disables_warnings(&src_of("@no-warnings")));
    }

    #[test]
    fn empty_source_has_no_marker() {
        assert!(!source_disables_warnings(&src_of("")));
    }

    fn src_of(content: &str) -> NamedSource<Arc<String>> {
        NamedSource::new("test.zn", Arc::new(content.to_string()))
    }
}
