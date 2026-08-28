use std::path::{Path, PathBuf};
use std::process::Command;

/// A parsed target triple (`arch-vendor-os-env`, e.g. `x86_64-unknown-linux-gnu`).
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

    pub fn is_windows(&self) -> bool {
        self.os == "windows"
    }

    pub fn is_wasm(&self) -> bool {
        self.arch == "wasm32" || self.arch == "wasm64"
    }
}

/// How the resulting object files are turned into a binary.
#[derive(Debug)]
enum Toolchain {
    /// A compiler/linker invoked as-is (gcc family, wasm-ld, native cc...).
    Command { program: String, args: Vec<String> },
    /// `clang` style driver with a `--target <triple>` pair.
    Clang {
        program: String,
        triple: String,
        args: Vec<String>,
    },
    /// MSVC `link.exe` with resolved library search paths and C runtime libs.
    Msvc {
        link: PathBuf,
        lib_paths: Vec<PathBuf>,
        target_machine: &'static str,
    },
}

impl Toolchain {
    fn command(program: &str, args: &[&str]) -> Self {
        Toolchain::Command {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        }
    }

    fn clang(program: &str, triple: &str, args: &[&str]) -> Self {
        Toolchain::Clang {
            program: program.to_owned(),
            triple: triple.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        }
    }

    fn msvc(link: PathBuf, lib_paths: Vec<PathBuf>, target_machine: &'static str) -> Self {
        Toolchain::Msvc {
            link,
            lib_paths,
            target_machine,
        }
    }

    fn display_name(&self) -> String {
        match self {
            Toolchain::Command { program, .. } => program.clone(),
            Toolchain::Clang {
                program, triple, ..
            } => format!("{program} (target {triple})"),
            Toolchain::Msvc { .. } => "link.exe (MSVC)".to_owned(),
        }
    }

    fn build_command(&self, objects: &[PathBuf], extra: &[PathBuf], output: &Path) -> Command {
        match self {
            Toolchain::Command { program, args } => {
                let mut command = Command::new(program);
                command.args(objects).args(extra).args(args);
                command.arg("-o").arg(output);
                command
            }
            Toolchain::Clang {
                program,
                triple,
                args,
            } => {
                let mut command = Command::new(program);
                command.args(objects).args(extra).args(args);
                command.arg("-target").arg(triple);
                command.arg("-o").arg(output);
                command
            }
            Toolchain::Msvc {
                link,
                lib_paths,
                target_machine,
            } => {
                let mut command = Command::new(link);
                command.args(objects).args(extra);
                for lib_path in lib_paths {
                    command.arg(format!("/LIBPATH:{}", lib_path.display()));
                }
                command
                    .arg(format!("/OUT:{}", output.display()))
                    .arg("/SUBSYSTEM:CONSOLE")
                    .arg(format!("/MACHINE:{target_machine}"))
                    .arg("/ENTRY:mainCRTStartup")
                    .arg("/NODEFAULTLIB:msvcrt.lib")
                    .arg("libcmt.lib")
                    .arg("kernel32.lib")
                    .arg("user32.lib");
                command
            }
        }
    }
}

/// Links object files into a binary for a specific target.
#[derive(Debug)]
pub struct ObjectLinker {
    target: Target,
    toolchain: Toolchain,
}

impl ObjectLinker {
    /// Resolves the toolchain able to link for `triple`.
    ///
    /// Fails with a descriptive message when no usable toolchain is found.
    pub fn detect(triple: &str) -> Result<Self, String> {
        let target = Target::parse(triple);
        let toolchain = Self::resolve(&target)?;

        Ok(Self { target, toolchain })
    }

    pub fn name(&self) -> String {
        self.toolchain.display_name()
    }

    /// The object file extension LLVM should use for this target.
    pub fn object_extension(&self) -> &'static str {
        if self.target.is_windows() { "obj" } else { "o" }
    }

    /// Applies the binary extension for the target (`.exe` on Windows, `.wasm`
    /// for wasm) unless the user already supplied it.
    pub fn output_path(&self, requested: &Path) -> PathBuf {
        Self::apply_extension(&self.target, requested)
    }

    /// Links `objects` into `output`.
    ///
    /// Returns the actual output path on success, or the linker's stderr/stdout
    /// on failure.
    pub fn link(
        &self,
        objects: &[PathBuf],
        output: &Path,
        extra: &[PathBuf],
    ) -> Result<PathBuf, String> {
        let output = Self::apply_extension(&self.target, output);

        let name = self.toolchain.display_name();
        let result = self
            .toolchain
            .build_command(objects, extra, &output)
            .output()
            .map_err(|err| format!("failed to spawn linker `{name}`: {err}"))?;

        if result.status.success() {
            return Ok(output);
        }

        let body = if result.stderr.is_empty() {
            result.stdout
        } else {
            result.stderr
        };

        Err(String::from_utf8_lossy(&body).into_owned())
    }

    /// Same as [`Self::output_path`], usable without a resolved toolchain.
    pub fn output_path_for(triple: &str, requested: &Path) -> PathBuf {
        Self::apply_extension(&Target::parse(triple), requested)
    }

    fn apply_extension(target: &Target, requested: &Path) -> PathBuf {
        let extension = if target.is_windows() {
            Some("exe")
        } else if target.is_wasm() {
            Some("wasm")
        } else {
            None
        };

        let Some(extension) = extension else {
            return requested.to_path_buf();
        };

        if requested
            .extension()
            .is_some_and(|existing| existing.to_string_lossy().eq_ignore_ascii_case(extension))
        {
            return requested.to_path_buf();
        }

        let mut name = requested.as_os_str().to_os_string();
        name.push(".");
        name.push(extension);
        PathBuf::from(name)
    }

    fn resolve(target: &Target) -> Result<Toolchain, String> {
        if target.is_wasm() {
            return Self::resolve_wasm(target);
        }
        if target.is_windows() {
            return if target.env == "msvc" {
                Self::resolve_msvc(target)
            } else {
                Self::resolve_mingw(target)
            };
        }
        if target.os == "darwin" || target.os == "macos" {
            return Self::resolve_darwin(target);
        }
        if target.os == "linux" {
            return if target.env == "musl" {
                Self::resolve_linux_musl(target)
            } else {
                Self::resolve_linux_gnu(target)
            };
        }
        if target.os == "android" {
            return Self::resolve_android(target);
        }
        if target.os == "freebsd" {
            return Self::resolve_freebsd(target);
        }

        Self::resolve_fallback(target)
    }

    fn resolve_linux_gnu(target: &Target) -> Result<Toolchain, String> {
        let mut tried = Vec::new();

        if let Some(gcc) = Self::linux_gnu_gcc_alias(target) {
            tried.push(gcc.clone());
            if probe(&gcc) {
                return Ok(Toolchain::command(&gcc, &["-lm"]));
            }
        }

        if Self::is_native(target) {
            for candidate in ["cc", "gcc", "clang"] {
                tried.push(candidate.to_owned());
                if probe(candidate) {
                    return Ok(Toolchain::command(candidate, &["-lm"]));
                }
            }
        }

        if probe("clang") {
            return Ok(Toolchain::clang("clang", &target.triple, &["-lm"]));
        }
        tried.push("clang".to_owned());

        Err(Self::not_found(target, &tried))
    }

    fn resolve_linux_musl(target: &Target) -> Result<Toolchain, String> {
        let flags: &[&str] = &["-lm", "-static"];
        let candidates: &[&str] = match target.arch.as_str() {
            "x86_64" => &["x86_64-linux-musl-gcc", "musl-gcc"],
            _ => &["musl-gcc"],
        };

        let mut tried = Vec::new();
        for gcc in candidates {
            tried.push((*gcc).to_owned());
            if probe(gcc) {
                return Ok(Toolchain::command(gcc, flags));
            }
        }

        if Self::is_musl_host(target) {
            for candidate in ["cc", "gcc", "clang"] {
                tried.push(candidate.to_owned());
                if probe(candidate) {
                    return Ok(Toolchain::command(candidate, flags));
                }
            }
        }

        if probe("clang") {
            return Ok(Toolchain::clang("clang", &target.triple, flags));
        }
        tried.push("clang".to_owned());

        Err(Self::not_found(target, &tried))
    }

    fn resolve_mingw(target: &Target) -> Result<Toolchain, String> {
        let prefixed = format!("{}-w64-mingw32-gcc", target.arch);
        if probe(&prefixed) {
            return Ok(Toolchain::command(&prefixed, &[]));
        }

        if Self::is_native(target) {
            for candidate in ["gcc", "cc"] {
                if probe(candidate) {
                    return Ok(Toolchain::command(candidate, &[]));
                }
            }
        }

        if probe("clang") {
            return Ok(Toolchain::clang("clang", &target.triple, &[]));
        }

        let tried = [prefixed, "clang".to_owned()];
        Err(Self::not_found(target, &tried))
    }

    fn resolve_msvc(target: &Target) -> Result<Toolchain, String> {
        if Self::host_os() != "windows" {
            return Err(format!(
                "MSVC target `{}` requires MSVC tools and the Windows SDK, which are only \
                 available on Windows; install MinGW-w64 and use `--target {}-pc-windows-gnu` instead",
                target.triple, target.arch
            ));
        }

        let (lib_arch, target_machine) = match target.arch.as_str() {
            "x86_64" => ("x64", "X64"),
            "i686" => ("x86", "X86"),
            other => return Err(format!("unsupported MSVC architecture `{other}`")),
        };

        let Some((link, lib_paths)) = Self::locate_msvc(lib_arch) else {
            return Err(
                "MSVC toolchain not found: install the \"Desktop development with C++\" workload \
                 via Visual Studio Build Tools, or run the compiler from a Developer Command Prompt"
                    .to_owned(),
            );
        };

        Ok(Toolchain::msvc(link, lib_paths, target_machine))
    }

    fn resolve_darwin(target: &Target) -> Result<Toolchain, String> {
        let prefixes: &[&str] = match target.arch.as_str() {
            "x86_64" => &[
                "x86_64-apple-darwin-clang",
                "o64-clang",
                "x86_64-apple-darwin-gcc",
            ],
            "aarch64" => &[
                "aarch64-apple-darwin-clang",
                "oa64-clang",
                "aarch64-apple-darwin-gcc",
            ],
            _ => &[],
        };

        let mut tried = Vec::new();
        for prefix in prefixes {
            tried.push((*prefix).to_owned());
            if probe(prefix) {
                return Ok(Toolchain::command(prefix, &["-lm"]));
            }
        }

        if Self::is_native(target) {
            for candidate in ["clang", "cc", "gcc"] {
                tried.push(candidate.to_owned());
                if probe(candidate) {
                    return Ok(Toolchain::command(candidate, &["-lm"]));
                }
            }
        }

        if probe("clang") {
            return Ok(Toolchain::clang("clang", &target.triple, &["-lm"]));
        }
        tried.push("clang".to_owned());

        Err(Self::not_found(target, &tried))
    }

    fn resolve_android(target: &Target) -> Result<Toolchain, String> {
        let prefixed = format!("{}-clang", target.arch);
        if probe(&prefixed) {
            return Ok(Toolchain::command(&prefixed, &[]));
        }

        if let Some(ndk) = Self::android_ndk() {
            let host = Self::ndk_host();
            let prebuilt = ndk
                .join("toolchains")
                .join("llvm")
                .join("prebuilt")
                .join(host);

            let clang = prebuilt.join("bin").join(&prefixed);
            if clang.is_file() {
                return Ok(Toolchain::command(&clang.display().to_string(), &[]));
            }

            let clang = prebuilt.join("bin").join("clang");
            let sysroot = prebuilt.join("sysroot");
            if clang.is_file() && sysroot.is_dir() {
                let sysroot_arg = sysroot.display().to_string();
                return Ok(Toolchain::clang(
                    &clang.display().to_string(),
                    &target.triple,
                    &["--sysroot", sysroot_arg.as_str()],
                ));
            }
        }

        Err(format!(
            "Android NDK not found for target `{}`: set `ANDROID_NDK_HOME`/`ANDROID_NDK_ROOT` \
             or install the NDK",
            target.triple
        ))
    }

    fn resolve_freebsd(target: &Target) -> Result<Toolchain, String> {
        let candidates = ["x86_64-unknown-freebsd-gcc", "x86_64-freebsd-gcc"];

        for gcc in &candidates {
            if probe(gcc) {
                return Ok(Toolchain::command(gcc, &["-lm"]));
            }
        }

        if probe("clang") {
            return Ok(Toolchain::clang("clang", &target.triple, &["-lm"]));
        }

        let mut tried = candidates
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>();
        tried.push("clang".to_owned());
        Err(Self::not_found(target, &tried))
    }

    fn resolve_wasm(target: &Target) -> Result<Toolchain, String> {
        // WASI prefers clang with a sysroot so libc symbols resolve properly;
        // fall through to the libc-free path when no sysroot is installed.
        if (target.os == "wasip1" || target.os == "wasi")
            && let Some(sysroot) = Self::wasi_sysroot()
        {
            let sysroot_arg = sysroot.display().to_string();
            return Ok(Toolchain::clang(
                "clang",
                &target.triple,
                &["--sysroot", sysroot_arg.as_str(), "-Wl,--allow-undefined"],
            ));
        }

        if probe("wasm-ld") {
            return Ok(Toolchain::command(
                "wasm-ld",
                &["--allow-undefined", "--no-entry", "--export-all"],
            ));
        }

        if probe("clang") {
            return Ok(Toolchain::clang(
                "clang",
                &target.triple,
                &[
                    "-nostdlib",
                    "-Wl,--allow-undefined",
                    "-Wl,--no-entry",
                    "-Wl,--export-all",
                ],
            ));
        }

        Err(format!(
            "no wasm linker found for target `{}`: install `wasm-ld` (LLVM wasm tools) or clang",
            target.triple
        ))
    }

    fn resolve_fallback(target: &Target) -> Result<Toolchain, String> {
        if Self::is_native(target) {
            for candidate in ["cc", "gcc", "clang"] {
                if probe(candidate) {
                    return Ok(Toolchain::command(candidate, &[]));
                }
            }
        }

        if probe("clang") {
            return Ok(Toolchain::clang("clang", &target.triple, &[]));
        }

        Err(Self::not_found(target, &["cc", "gcc", "clang"]))
    }

    fn not_found<S: AsRef<str>>(target: &Target, tried: &[S]) -> String {
        let tried = tried
            .iter()
            .map(|candidate| candidate.as_ref())
            .collect::<Vec<_>>()
            .join(", ");

        format!(
            "no linker found for target `{}` (tried: {tried})",
            target.triple
        )
    }

    fn linux_gnu_gcc_alias(target: &Target) -> Option<String> {
        match target.arch.as_str() {
            "x86_64" => Some("x86_64-linux-gnu-gcc".to_owned()),
            "i686" => Some("i686-linux-gnu-gcc".to_owned()),
            "aarch64" => Some("aarch64-linux-gnu-gcc".to_owned()),
            _ => None,
        }
    }

    fn is_native(target: &Target) -> bool {
        target.os == Self::host_os() && target.arch == Self::host_arch()
    }

    fn is_musl_host(target: &Target) -> bool {
        if std::env::consts::OS != "linux" {
            return false;
        }

        let loader = match target.arch.as_str() {
            "x86_64" => "/lib/ld-musl-x86_64.so.1",
            "i686" => "/lib/ld-musl-i386.so.1",
            "aarch64" => "/lib/ld-musl-aarch64.so.1",
            _ => return false,
        };

        Path::new(loader).exists()
    }

    fn host_os() -> &'static str {
        match std::env::consts::OS {
            "macos" => "darwin",
            other => other,
        }
    }

    fn host_arch() -> &'static str {
        match std::env::consts::ARCH {
            "x86" => "i686",
            "arm64" => "aarch64",
            other => other,
        }
    }

    fn wasi_sysroot() -> Option<PathBuf> {
        for var in ["WASI_SYSROOT", "WASI_SDK_PATH"] {
            if let Ok(path) = std::env::var(var) {
                let path = PathBuf::from(path);
                if path.is_dir() {
                    return Some(path);
                }
            }
        }
        None
    }

    fn android_ndk() -> Option<PathBuf> {
        for var in ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT", "NDK_HOME"] {
            if let Ok(path) = std::env::var(var) {
                let path = PathBuf::from(path);
                if path.is_dir() {
                    return Some(path);
                }
            }
        }
        None
    }

    fn ndk_host() -> &'static str {
        let os = match std::env::consts::OS {
            "macos" => "darwin",
            other => other,
        };
        let arch = match std::env::consts::ARCH {
            "aarch64" | "arm64" => "arm64",
            _ => "x86_64",
        };
        match (os, arch) {
            ("linux", "x86_64") => "linux-x86_64",
            ("linux", "arm64") => "linux-arm64",
            ("darwin", "x86_64") => "darwin-x86_64",
            ("darwin", "arm64") => "darwin-arm64",
            ("windows", "x86_64") => "windows-x86_64",
            _ => "linux-x86_64",
        }
    }

    // MSVC toolchain discovery (Windows only, in practice).

    fn locate_msvc(lib_arch: &str) -> Option<(PathBuf, Vec<PathBuf>)> {
        let host_arch = if cfg!(target_pointer_width = "64") {
            "x64"
        } else {
            "x86"
        };

        let install = Self::msvc_install_dir()?;

        let msvc_root = install.join("VC").join("Tools").join("MSVC");
        let version = Self::newest_version_dir(&msvc_root)?;
        let msvc = msvc_root.join(&version);

        let link = msvc.join(format!("bin/Host{host_arch}/{lib_arch}/link.exe"));
        if !link.is_file() {
            return None;
        }

        let kits = install.join("Windows Kits").join("10").join("Lib");
        let sdk_version = Self::newest_version_dir(&kits)?;

        let mut lib_paths = vec![msvc.join("lib").join(lib_arch)];
        lib_paths.push(kits.join(&sdk_version).join("ucrt").join(lib_arch));
        lib_paths.push(kits.join(&sdk_version).join("um").join(lib_arch));

        Some((link, lib_paths))
    }

    fn msvc_install_dir() -> Option<PathBuf> {
        if let Ok(vc_install) = std::env::var("VCINSTALLDIR") {
            let root = PathBuf::from(vc_install);
            return Some(match root.file_name().and_then(|name| name.to_str()) {
                Some("VC") => root.parent()?.to_path_buf(),
                _ => root,
            });
        }

        const VSWHERE: &str =
            r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe";
        if Path::new(VSWHERE).is_file() {
            let output = Command::new(VSWHERE)
                .args([
                    "-latest",
                    "-products",
                    "*",
                    "-requires",
                    "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
                    "-property",
                    "installationPath",
                ])
                .output()
                .ok()?;

            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
                if !path.is_empty() {
                    return Some(PathBuf::from(path));
                }
            }
        }

        const VERSIONS: &[&str] = &[
            r"C:\Program Files\Microsoft Visual Studio\2022\Community",
            r"C:\Program Files\Microsoft Visual Studio\2022\Professional",
            r"C:\Program Files\Microsoft Visual Studio\2022\Enterprise",
            r"C:\Program Files\Microsoft Visual Studio\2022\BuildTools",
            r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Community",
            r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Professional",
            r"C:\Program Files (x86)\Microsoft Visual Studio\2019\Enterprise",
            r"C:\Program Files (x86)\Microsoft Visual Studio\2019\BuildTools",
        ];

        VERSIONS
            .iter()
            .map(PathBuf::from)
            .find(|path| path.join("VC").is_dir())
    }

    /// Finds the newest numeric version subdirectory under `parent`.
    fn newest_version_dir(parent: &Path) -> Option<String> {
        let entries = std::fs::read_dir(parent).ok()?;

        entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.split('.').all(|part| part.parse::<u64>().is_ok()))
            .max_by(|a, b| Self::version_key(a).cmp(&Self::version_key(b)))
    }

    fn version_key(name: &str) -> Vec<u64> {
        name.split('.')
            .filter_map(|part| part.parse().ok())
            .collect()
    }
}

fn probe(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_target_triple() {
        let target = Target::parse("x86_64-pc-windows-msvc");
        assert_eq!(target.arch, "x86_64");
        assert_eq!(target.os, "windows");
        assert_eq!(target.env, "msvc");
        assert!(target.is_windows());
    }

    #[test]
    fn parses_short_triple_without_env() {
        let target = Target::parse("x86_64-apple-darwin");
        assert_eq!(target.arch, "x86_64");
        assert_eq!(target.os, "darwin");
        assert_eq!(target.env, "");
    }

    #[test]
    fn windows_binary_gets_exe_extension() {
        let output = ObjectLinker::output_path_for("x86_64-pc-windows-gnu", Path::new("out"));
        assert_eq!(output, PathBuf::from("out.exe"));
    }

    #[test]
    fn windows_binary_keeps_existing_exe_extension() {
        let output = ObjectLinker::output_path_for("x86_64-pc-windows-msvc", Path::new("out.exe"));
        assert_eq!(output, PathBuf::from("out.exe"));
    }

    #[test]
    fn wasm_binary_gets_wasm_extension() {
        let output = ObjectLinker::output_path_for("wasm32-unknown-unknown", Path::new("out"));
        assert_eq!(output, PathBuf::from("out.wasm"));
    }

    #[test]
    fn non_windows_binary_keeps_extension() {
        let output =
            ObjectLinker::output_path_for("x86_64-unknown-linux-gnu", Path::new("output.bin"));
        assert_eq!(output, PathBuf::from("output.bin"));
    }

    #[test]
    fn msvc_target_rejected_off_windows() {
        let error = ObjectLinker::detect("x86_64-pc-windows-msvc").unwrap_err();
        assert!(error.contains("windows-gnu"), "unexpected error: {error}");
    }
}
