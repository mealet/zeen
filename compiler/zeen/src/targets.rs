pub const SUPPORTED_TARGETS: &[&str] = &[
    // Linux
    "x86_64-unknown-linux-gnu",
    "x86_64-unknown-linux-musl",
    "i686-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    // Mac
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    // Win
    "x86_64-pc-windows-gnu",
    "x86_64-pc-windows-msvc",
    "i686-pc-windows-gnu",
    "i686-pc-windows-msvc",
    // Wasm
    "wasm32-unknown-unknown",
    "wasm32-unknown-wasip1",
    // Others
    "x86_64-unknown-freebsd",
    "aarch64-linux-android",
];

pub fn is_supported(triple: &str) -> bool {
    SUPPORTED_TARGETS.contains(&triple)
}

/// Returns the host target triple, normalized to the naming scheme used in
/// [`SUPPORTED_TARGETS`].
pub fn host_target() -> String {
    let arch = match std::env::consts::ARCH {
        "x86" => "i686",
        "arm64" => "aarch64",
        other => other,
    };

    match std::env::consts::OS {
        "linux" => format!("{arch}-unknown-linux-gnu"),
        "macos" => format!("{arch}-apple-darwin"),
        "windows" => format!("{arch}-pc-windows-msvc"),
        other => format!("{arch}-unknown-{other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_targets_are_supported() {
        for triple in SUPPORTED_TARGETS {
            assert!(is_supported(triple), "{triple} should be supported");
        }
    }

    #[test]
    fn unknown_target_is_rejected() {
        assert!(!is_supported("powerpc64le-unknown-linux-gnu"));
    }

    #[test]
    fn host_target_uses_supported_naming() {
        let host = host_target();
        let parts: Vec<&str> = host.split('-').collect();
        assert!(parts.len() >= 3, "unexpected triple {host}");
        assert!(!parts[0].is_empty());
        assert!(!parts[1].is_empty());
        assert!(!parts[2].is_empty());
    }
}
