use std::path::{Path, PathBuf};

pub struct ObjectLinker;

impl ObjectLinker {
    pub fn detect_compiler() -> Option<String> {
        let candidates = match std::env::consts::OS {
            "windows" => ["link", "gcc", "clang"],
            "macos" => ["clang", "gcc", "cc"],
            _ => ["gcc", "clang", "cc"],
        };

        for compiler in candidates {
            if std::process::Command::new(compiler)
                .arg("--version")
                .output()
                .is_ok()
            {
                return Some(compiler.to_string());
            }
        }

        None
    }

    pub fn link(objects: &[PathBuf], output: &Path, extra: &[PathBuf]) -> Result<String, String> {
        let mut output_path = output.to_path_buf();
        if cfg!(windows) && output_path.extension().is_none_or(|e| e != "exe") {
            output_path.set_extension("exe");
        }

        let Some(compiler) = Self::detect_compiler() else {
            return Err(String::from(
                "No supported C compilers found in system. Recommended: gcc/clang",
            ));
        };

        let object_args = objects
            .iter()
            .map(|object| object.as_os_str().to_os_string())
            .collect::<Vec<_>>();
        let extra_args = extra
            .iter()
            .map(|input| input.as_os_str().to_os_string())
            .collect::<Vec<_>>();

        let linker_output = match compiler.as_str() {
            "link" => {
                let msvc_path = r"C:\Program Files (x86)\Microsoft Visual Studio\2019\BuildTools\VC\Tools\MSVC\14.29.30133\lib\x64";
                let sdk_um_path = r"C:\Program Files (x86)\Windows Kits\10\lib\10.0.22000.0\um\x64";
                let sdk_ucrt_path =
                    r"C:\Program Files (x86)\Windows Kits\10\lib\10.0.22000.0\ucrt\x64";

                std::process::Command::new(&compiler)
                    .args(&object_args)
                    .args(&extra_args)
                    .arg(format!("/OUT:{}", output_path.display()))
                    .arg(format!("/LIBPATH:{msvc_path}"))
                    .arg(format!("/LIBPATH:{sdk_um_path}"))
                    .arg(format!("/LIBPATH:{sdk_ucrt_path}"))
                    .arg("/SUBSYSTEM:CONSOLE")
                    .arg("/MACHINE:X64")
                    .arg("/ENTRY:mainCRTStartup")
                    .arg("/NODEFAULTLIB:msvcrt.lib")
                    .arg("libcmt.lib")
                    .arg("kernel32.lib")
                    .arg("user32.lib")
                    .output()
            }
            _ => {
                let mut command = std::process::Command::new(&compiler);
                command
                    .args(&object_args)
                    .args(&extra_args)
                    .arg("-fPIC")
                    .arg("-lm")
                    .arg("-o")
                    .arg(&output_path);
                if std::env::consts::OS == "windows" {
                    command.arg("-lmsvcrt").arg("-lucrt").arg("-lgcc");
                }
                command.output()
            }
        };

        let output = linker_output.map_err(|err| err.to_string())?;
        if output.status.success() {
            return Ok(compiler);
        }

        let error_message = if output.stderr.is_empty() {
            String::from_utf8_lossy(&output.stdout).to_string()
        } else {
            String::from_utf8_lossy(&output.stderr).to_string()
        };
        Err(error_message)
    }
}
