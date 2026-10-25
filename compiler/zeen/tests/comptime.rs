use assert_cmd::Command;
use std::{fs, process::Output};

struct CompileResult {
    success: bool,
    stdout: String,
    stderr: String,
}

fn compile(name: &str, source: &str) -> CompileResult {
    let dir = std::env::temp_dir().join(format!("zeen_comptime_{}_{name}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("main.zn");
    fs::write(&path, source).unwrap();

    let output: Output = Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .arg(&path)
        .arg("--check")
        .assert()
        .get_output()
        .clone();

    let _ = fs::remove_dir_all(&dir);

    CompileResult {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).replace('\r', ""),
        stderr: String::from_utf8_lossy(&output.stderr).replace('\r', ""),
    }
}

#[test]
fn prints_block_and_function_prefixes() {
    let source = r#"comptime fn emit(n: comptime_int) comptime_int {
  @println("function {}", n);
  return n;
}

fn main() {
  comptime {
    @println("block");
    emit(1);
  };
}
"#;

    let result = compile("prefixes", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("[comptime][main.zn:7] block"),
        "stdout: {}",
        result.stdout
    );
    assert!(
        result
            .stdout
            .contains("[comptime emit][main.zn:9] function 1"),
        "stdout: {}",
        result.stdout
    );
}

#[test]
fn formats_values_like_runtime() {
    let source = r#"fn main() {
  comptime {
    @println("{{}} {} {:?}", 1, "hi");
    @println("hex {hex} oct {oct} bin {bin}", 255, 8, 5);
    @println("float {} {:.2}", 1.5, 1.5);
    @println("debug {:?} {:?} {:?}", 'a', true, 1);
    let text = @format("{}", 42);
    @println("format {}", text);
  };
}
"#;

    let result = compile("formatting", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("[comptime][main.zn:2] {} 1 \"hi\""),
        "stdout: {}",
        result.stdout
    );
    assert!(
        result
            .stdout
            .contains("[comptime][main.zn:2] hex ff oct 10 bin 101"),
        "stdout: {}",
        result.stdout
    );
    assert!(
        result
            .stdout
            .contains("[comptime][main.zn:2] float 1.500000 1.50"),
        "stdout: {}",
        result.stdout
    );
    assert!(
        result
            .stdout
            .contains("[comptime][main.zn:2] debug 'a' true 1"),
        "stdout: {}",
        result.stdout
    );
    assert!(
        result.stdout.contains("[comptime][main.zn:2] format 42"),
        "stdout: {}",
        result.stdout
    );
}

#[test]
fn nested_block_keeps_trailing_value() {
    let source = r#"fn main() i32 {
  comptime {
    let value = comptime {
      @println("inner");
      41
    };
    @println("result {}", value + 1);
  };
  return 0;
}
"#;

    let result = compile("nested", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("[comptime][main.zn:3] inner"),
        "stdout: {}",
        result.stdout
    );
    assert!(
        result.stdout.contains("[comptime][main.zn:2] result 42"),
        "stdout: {}",
        result.stdout
    );
}

#[test]
fn rejects_unknown_specifier() {
    let result = compile(
        "unknown_specifier",
        "fn main() {\n  comptime {\n    @println(\"{:q}\", 1);\n  };\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::format_parse_error"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result.stderr.contains("unknown format specifier"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_unclosed_brace() {
    let result = compile(
        "unclosed_brace",
        "fn main() {\n  comptime {\n    @println(\"{\");\n  };\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("unclosed '{' in format string"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_placeholder_count_mismatch() {
    let result = compile(
        "placeholder_count",
        "fn main() {\n  comptime {\n    @println(\"{} {}\", 1);\n  };\n}\n",
    );

    assert!(!result.success);
    assert!(
        result
            .stderr
            .contains("format string provides 2 placeholders, but found 1"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_integer_specifier_for_string() {
    let result = compile(
        "integer_specifier",
        "fn main() {\n  comptime {\n    @println(\"{hex}\", \"text\");\n  };\n}\n",
    );

    assert!(!result.success);
    assert!(
        result
            .stderr
            .contains("format requires integer type, but found `comptime_str`"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_float_specifier_for_int() {
    let result = compile(
        "float_specifier",
        "fn main() {\n  comptime {\n    @println(\"{:.2}\", 3);\n  };\n}\n",
    );

    assert!(!result.success);
    assert!(
        result
            .stderr
            .contains("format requires float type, but found `comptime_int`"),
        "stderr: {}",
        result.stderr
    );
}
