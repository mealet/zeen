use std::{path::Path, path::PathBuf, sync::Arc};

use miette::NamedSource;

const MARKER: &[u8] = b"// @no-warnings";
const CORE_MODULE_PREFIX: &str = "core.";

pub fn source_disables_warnings(source: &NamedSource<Arc<String>>, packages: &[PathBuf]) -> bool {
    find_marker(source.inner().as_bytes()) || is_quiet_module(source, packages)
}

fn is_quiet_module(source: &NamedSource<Arc<String>>, packages: &[PathBuf]) -> bool {
    let name = source.name();

    if name.starts_with(CORE_MODULE_PREFIX) {
        return true;
    }

    let path = Path::new(name);
    packages.iter().any(|pkg| path.starts_with(pkg))
}

fn find_marker(data: &[u8]) -> bool {
    data.windows(MARKER.len()).any(|window| window == MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg_dirs() -> Vec<PathBuf> {
        vec![PathBuf::from(r"\\?\Z:\fake-root\httplib")]
    }

    #[test]
    fn detects_line_marker() {
        assert!(source_disables_warnings(&src_of("// @no-warnings"), &[]));
        assert!(source_disables_warnings(
            &NamedSource::new(
                "test.zn",
                Arc::new("let x = 1;\n// @no-warnings\nlet y = 2;".to_string())
            ),
            &[]
        ));
    }

    #[test]
    fn requires_comment_prefix() {
        assert!(!source_disables_warnings(&src_of("let x = 1;"), &[]));
        assert!(!source_disables_warnings(&src_of("@no-warnings"), &[]));
    }

    #[test]
    fn empty_source_has_no_marker() {
        assert!(!source_disables_warnings(&src_of(""), &[]));
    }

    #[test]
    fn core_module_is_quiet() {
        assert!(source_disables_warnings(&src_named("core.option", ""), &[]));
        assert!(!source_disables_warnings(&src_named("main.zn", ""), &[]));
        assert!(!source_disables_warnings(&src_named("std.string", ""), &[]));
    }

    #[test]
    fn package_sources_are_quiet() {
        let packages = pkg_dirs();
        let pkg = packages[0].clone();

        assert!(source_disables_warnings(
            &src_named(&pkg.join("index.zn").to_string_lossy(), ""),
            &packages
        ));
        assert!(source_disables_warnings(
            &src_named(&pkg.join("folder").join("lib.zn").to_string_lossy(), ""),
            &packages
        ));
        assert!(!source_disables_warnings(
            &src_named(&pkg.parent().unwrap().join("main.zn").to_string_lossy(), ""),
            &packages
        ));
    }

    fn src_named(name: &str, content: &str) -> NamedSource<Arc<String>> {
        NamedSource::new(name, Arc::new(content.to_string()))
    }

    fn src_of(content: &str) -> NamedSource<Arc<String>> {
        src_named("test.zn", content)
    }
}
