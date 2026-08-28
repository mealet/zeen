use std::path::Path;

/// A parsed target triple (`arch[-vendor]-os[-env]`, e.g. `x86_64-unknown-linux-gnu`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub triple: String,
    pub arch: String,
    pub os: String,
    pub env: String,
}

impl Target {
    pub fn parse(triple: &str) -> Self {
        let mut parts = triple.split('-');
        let arch = parts.next().unwrap_or(triple).to_owned();
        let _vendor = parts.next().unwrap_or("unknown");
        let os = parts.next().unwrap_or("unknown").to_owned();
        let env = parts.next().unwrap_or_default().to_owned();

        Self {
            triple: triple.to_owned(),
            arch,
            os,
            env,
        }
    }

    /// The host target, normalized to common triple naming: `macos` becomes
    /// `darwin`, `arm64` becomes `aarch64`, and Linux reports `-musl` when the
    /// host runs the musl libc.
    pub fn host() -> Self {
        let arch = match std::env::consts::ARCH {
            "x86" => "i686",
            "arm64" => "aarch64",
            other => other,
        };

        let (vendor, os, env) = match std::env::consts::OS {
            "linux" => (
                "unknown",
                "linux",
                if Self::on_musl(arch) { "musl" } else { "gnu" },
            ),
            "macos" => ("apple", "darwin", ""),
            "windows" => ("pc", "windows", "msvc"),
            other => ("unknown", other, ""),
        };

        let triple = if env.is_empty() {
            format!("{arch}-{vendor}-{os}")
        } else {
            format!("{arch}-{vendor}-{os}-{env}")
        };

        Self {
            triple,
            arch: arch.to_owned(),
            os: os.to_owned(),
            env: env.to_owned(),
        }
    }

    pub fn is_windows(&self) -> bool {
        self.os == "windows"
    }

    pub fn is_wasm(&self) -> bool {
        self.arch == "wasm32" || self.arch == "wasm64"
    }

    fn on_musl(arch: &str) -> bool {
        if std::env::consts::OS != "linux" {
            return false;
        }

        let loader = match arch {
            "x86_64" => "/lib/ld-musl-x86_64.so.1",
            "i686" => "/lib/ld-musl-i386.so.1",
            "aarch64" => "/lib/ld-musl-aarch64.so.1",
            _ => return false,
        };

        Path::new(loader).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_triple() {
        let target = Target::parse("x86_64-pc-windows-msvc");
        assert_eq!(target.arch, "x86_64");
        assert_eq!(target.os, "windows");
        assert_eq!(target.env, "msvc");
        assert!(target.is_windows());
    }

    #[test]
    fn parses_triple_without_env() {
        let target = Target::parse("x86_64-apple-darwin");
        assert_eq!(target.arch, "x86_64");
        assert_eq!(target.os, "darwin");
        assert_eq!(target.env, "");
        assert!(!target.is_windows());
    }

    #[test]
    fn parses_wasi() {
        let target = Target::parse("wasm32-unknown-wasip1");
        assert!(target.is_wasm());
        assert_eq!(target.os, "wasip1");
    }

    #[test]
    fn host_uses_known_naming() {
        let host = Target::host();
        assert!(!host.triple.is_empty());
        assert!(!host.arch.is_empty());
        assert!(!host.os.is_empty());
        assert_ne!(host.os, "macos");
    }
}
