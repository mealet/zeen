use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() {
    // @ Core Modules Builtin Import
    {
        // project/lib/core
        let core_dir = Path::new("../../lib/core");

        println!("cargo:rerun-if-changed={}", core_dir.display());

        let mut output = String::new();

        output.push_str(
            r#"
pub struct CoreFile {
    pub name: &'static str,
    pub value: &'static str,
}

impl CoreFile {
    pub fn to_basic(&self) -> (&'static str, &'static str) {
        (self.name, self.value)
    }
}

pub static CORE_FILES: &[CoreFile] = &[
            "#,
        );

        for entry in fs::read_dir(core_dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            if path.extension().and_then(|ext| ext.to_str()) != Some("zn") {
                continue;
            }

            let stem = path.file_stem().unwrap().to_str().unwrap();
            let content = fs::read_to_string(&path).unwrap();

            output.push_str(&format!(
                r#"  CoreFile {{
    name: "core.{stem}",
    value: {content:?},
  }},"#
            ));

            output.push('\n');
        }

        output.push_str("];\n");

        let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
        fs::write(out_dir.join("core_files.rs"), output).unwrap();
    }
    // < Core Modules Builtin Import
}
