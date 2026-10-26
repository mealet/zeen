use assert_cmd::Command;
use std::{fs, process::Output};

struct CompileResult {
    success: bool,
    stdout: String,
    stderr: String,
}

fn compile_files(name: &str, files: &[(&str, &str)]) -> CompileResult {
    let dir = std::env::temp_dir().join(format!("zeen_comptime_{}_{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    for (file_name, source) in files {
        fs::write(dir.join(file_name), source).unwrap();
    }

    let path = dir.join("main.zn");

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

fn compile(name: &str, source: &str) -> CompileResult {
    compile_files(name, &[("main.zn", source)])
}

fn run(name: &str, source: &str) -> CompileResult {
    let dir = std::env::temp_dir().join(format!("zeen_comptime_run_{}_{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let path = dir.join("main.zn");
    fs::write(&path, source).unwrap();

    let binary = dir.join("program");

    let build: Output = Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .arg(&path)
        .arg(&binary)
        .assert()
        .get_output()
        .clone();

    let result = if !build.status.success() {
        CompileResult {
            success: false,
            stdout: String::new(),
            stderr: String::from_utf8_lossy(&build.stderr).replace('\r', ""),
        }
    } else {
        let executed = std::process::Command::new(&binary).output().unwrap();

        CompileResult {
            success: executed.status.success(),
            stdout: String::from_utf8_lossy(&executed.stdout).replace('\r', ""),
            stderr: String::from_utf8_lossy(&executed.stderr).replace('\r', ""),
        }
    };

    let _ = fs::remove_dir_all(&dir);

    result
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

#[test]
fn public_comptime_items_work_in_same_module() {
    let source = r#"pub comptime Pi = 3.14;

pub comptime fn double(n: comptime_int) comptime_int {
  return n * 2;
}

fn main() {
  comptime {
    @println("{} {}", double(21), Pi);
  };
}
"#;

    let result = compile("public_same_module", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("[comptime][main.zn:8] 42 3.140000"),
        "stdout: {}",
        result.stdout
    );
}

#[test]
fn imports_public_comptime_items() {
    let result = compile_files(
        "public_import",
        &[
            (
                "math.zn",
                r#"pub comptime Pi = 3.14;

pub comptime fn double(n: comptime_int) comptime_int {
  return n * 2;
}
"#,
            ),
            (
                "main.zn",
                r#"use math;

fn main() {
  comptime {
    @println("{} {}", double(21), Pi);
  };
}
"#,
            ),
        ],
    );

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("[comptime][main.zn:4] 42 3.140000"),
        "stdout: {}",
        result.stdout
    );
}

#[test]
fn rejects_private_comptime_const_across_modules() {
    let result = compile_files(
        "private_import",
        &[
            ("math.zn", "comptime Secret = 5;\n"),
            (
                "main.zn",
                "use math;\n\nfn main() {\n  comptime {\n    @println(\"{}\", Secret);\n  };\n}\n",
            ),
        ],
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::type_mismatch"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("comptime constant `Secret` is not public in the module that defines it"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_ambiguous_comptime_const() {
    let result = compile_files(
        "ambiguous_import",
        &[
            ("a.zn", "pub comptime Shared = 1;\n"),
            ("b.zn", "pub comptime Shared = 2;\n"),
            (
                "main.zn",
                "use a;\nuse b;\n\nfn main() {\n  comptime {\n    @println(\"{}\", Shared);\n  };\n}\n",
            ),
        ],
    );

    assert!(!result.success);
    assert!(
        result
            .stderr
            .contains("ambiguous comptime constant `Shared`"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn follows_public_use_chain() {
    let result = compile_files(
        "pub_use_chain",
        &[
            ("leaf.zn", "pub comptime Leaf = 7;\n"),
            ("mid.zn", "pub use leaf;\n"),
            (
                "main.zn",
                "use mid;\n\nfn main() {\n  comptime {\n    @println(\"{}\", Leaf);\n  };\n}\n",
            ),
        ],
    );

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("[comptime][main.zn:4] 7"),
        "stdout: {}",
        result.stdout
    );
}

#[test]
fn reflects_imported_public_type() {
    let result = compile_files(
        "reflect_import",
        &[
            (
                "math.zn",
                r#"pub struct Point {
  pub x: i32,
  pub y: i32,
}
"#,
            ),
            (
                "main.zn",
                r#"use math;

fn main() {
  comptime {
    let fields = @fields(Point);
    @println("{} {}", fields[0].name, fields[1].name);
  };
}
"#,
            ),
        ],
    );

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("[comptime][main.zn:4] x y"),
        "stdout: {}",
        result.stdout
    );
}

#[test]
fn rejects_step_limit() {
    let source = r#"comptime fn spin() comptime_int {
  let i = 0;
  while (i < 1000000) {
    i += 1;
  }
  return i;
}

fn main() {
  @println("{}", spin());
}
"#;

    let result = compile("step_limit", source);

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::limit_exceeded"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result.stderr.contains("comptime step limit exceeded"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_depth_limit() {
    let source = r#"comptime fn dive(n: comptime_int) comptime_int {
  return dive(n + 1);
}

fn main() {
  @println("{}", dive(0));
}
"#;

    let result = compile("depth_limit", source);

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::limit_exceeded"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("comptime evaluation depth limit exceeded"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_uninit_in_comptime() {
    let result = compile(
        "runtime_uninit",
        "fn main() {\n  comptime {\n    let x = @uninit();\n  };\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::runtime_only"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("`@uninit` is not available in comptime"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_record_values_in_runtime_code() {
    let source = r#"struct Point {
  x: i32,
  y: i32,
}

comptime Fields = @fields(Point);

fn main() {
  let fields = Fields;
  @println("{}", fields[0].name);
}
"#;

    let result = compile("runtime_record", source);

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::runtime_only"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("comptime Field values cannot be used in runtime code"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_splice_of_runtime_value() {
    let result = compile(
        "splice_runtime_value",
        "fn main() {\n  let x = $(1 + 2);\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::type_mismatch"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("`$()` in runtime code expects a zeen_expr, got comptime_int"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn reports_panic_call_trace() {
    let source = r#"comptime fn inner() comptime_int {
  @panic("boom {}", 1);
}

comptime fn outer() comptime_int {
  return inner();
}

fn main() {
  @println("{}", outer());
}
"#;

    let result = compile("panic_trace", source);

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::panic"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result.stderr.contains("boom 1"),
        "stderr: {}",
        result.stderr
    );
    assert_eq!(
        result.stderr.matches("`-- called from here").count(),
        2,
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_fields_on_enum() {
    let result = compile(
        "fields_on_enum",
        "enum Shape { Empty, Circle: f64 }\n\nfn main() {\n  comptime {\n    let fields = @fields(Shape);\n  };\n}\n",
    );

    assert!(!result.success);
    assert!(
        result
            .stderr
            .contains("@fields expects a struct type, use @variants for enums"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_variants_on_struct() {
    let result = compile(
        "variants_on_struct",
        "struct Point { x: i32 }\n\nfn main() {\n  comptime {\n    let variants = @variants(Point);\n  };\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("@variants expects an enum type"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_reflection_macro_in_runtime_code() {
    let result = compile(
        "reflection_runtime",
        "struct Point { x: i32 }\n\nfn main() {\n  let fields = @fields(Point);\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::typechecker::unknown_macro"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn accepts_comptime_types_in_signatures_and_locals() {
    let source = r#"comptime fn takes(
  a: comptime_int,
  b: comptime_float,
  c: comptime_bool,
  d: comptime_char,
  e: comptime_str,
  f: zeen_type,
) comptime_int {
  return a;
}

comptime fn returns_nothing() void {
  @println("nothing");
}

comptime fn keeps_expr(x: zeen_expr) zeen_expr {
  return x;
}

comptime fn keeps_decl(x: zeen_decl) zeen_decl {
  return x;
}

comptime fn total(xs: [3]comptime_int) comptime_int {
  return xs[0] + xs[1] + xs[2];
}

fn main() {
  comptime {
    let start: comptime_int = takes(1, 2.5, true, 'a', "text", @zeen_type(i32));
    let xs: [3]comptime_int = [1, 2, 3];
    @println("{} {}", start, total(xs));
    returns_nothing();
  };
}
"#;

    let result = compile("comptime_types", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(result.stdout.contains("1 6"), "stdout: {}", result.stdout);
}

#[test]
fn rejects_runtime_parameter_type() {
    let result = compile(
        "runtime_param",
        "comptime fn bad(x: i32) comptime_int {\n  return 1;\n}\n\nfn main() {}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::runtime_type"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result.stderr.contains(
            "runtime type `i32` is not allowed in comptime code, use `comptime_int` instead"
        ),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_runtime_return_type() {
    let result = compile(
        "runtime_return",
        "comptime fn bad() u32 {\n  return 1;\n}\n\nfn main() {}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::runtime_type"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("runtime type `u32` is not allowed in comptime code"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_runtime_local_annotation() {
    let result = compile(
        "runtime_local",
        "fn main() {\n  comptime {\n    let x: f64 = 1.5;\n  };\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::runtime_type"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result.stderr.contains(
            "runtime type `f64` is not allowed in comptime code, use `comptime_float` instead"
        ),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_void_parameter_type() {
    let result = compile(
        "void_param",
        "comptime fn bad(x: void) void {\n  @println(\"x\");\n}\n\nfn main() {}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::runtime_type"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("`void` is only allowed as a comptime function return type"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_runtime_pointer_type() {
    let result = compile(
        "runtime_pointer",
        "comptime fn bad(x: *i32) comptime_int {\n  return 1;\n}\n\nfn main() {}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::runtime_type"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("runtime type `*i32` is not allowed in comptime code"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn keeps_runtime_annotations_outside_comptime() {
    let result = compile(
        "runtime_annotation",
        "fn main() {\n  let x: i32 = 1;\n  @println(\"{}\", x);\n}\n",
    );

    assert!(result.success, "stderr: {}", result.stderr);
}

#[test]
fn comptime_struct_methods_and_arrays() {
    let source = r#"comptime struct Point {
  x: comptime_int,
  y: comptime_int,

  fn sum(self) comptime_int {
    return self.x + self.y;
  }

  fn offset(*self, dx: comptime_int) void {
    self.x += dx;
  }
}

comptime struct Row {
  cells: [3]Point,

  fn total(self) comptime_int {
    return self.cells[0].sum() + self.cells[1].sum() + self.cells[2].sum();
  }
}

comptime fn make(x: comptime_int) Point {
  return Point { .x = x, .y = x * 2 };
}

fn main() {
  comptime {
    let point = make(1);
    point.offset(4);
    @println("{} {}", point.x, point.sum());

    let row = Row { .cells = [make(1), make(2), make(3)] };
    @println("{} {}", row.total(), row.cells[2].y);

    let copy = point;
    copy.offset(10);
    @println("{} {}", point.x, copy.x);
  };
}
"#;

    let result = compile("comptime_struct", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("[comptime][main.zn:27] 5 7"),
        "stdout: {}",
        result.stdout
    );
    assert!(
        result.stdout.contains("[comptime][main.zn:27] 18 6"),
        "stdout: {}",
        result.stdout
    );
    assert!(
        result.stdout.contains("[comptime][main.zn:27] 5 15"),
        "stdout: {}",
        result.stdout
    );
}

#[test]
fn comptime_struct_accepts_nested_values() {
    let source = r#"comptime struct Inner {
  value: comptime_int,
}

comptime struct Middle {
  inner: Inner,
  name: comptime_str,
}

comptime struct Outer {
  middle: Middle,
  flag: comptime_bool,
}

comptime Value = Outer {
  .middle = Middle { .inner = Inner { .value = 3 }, .name = "nested" },
  .flag = true,
}.middle.inner.value;

fn main() {
  comptime {
    @println("{}", Value);
  };
}
"#;

    let result = compile("comptime_struct_nested", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("[comptime][main.zn:21] 3"),
        "stdout: {}",
        result.stdout
    );
}

#[test]
fn rejects_unknown_comptime_struct_field() {
    let result = compile(
        "comptime_struct_unknown_field",
        "comptime struct Point {\n  x: comptime_int,\n}\n\nfn main() {\n  comptime {\n    let point = Point { .x = 1, .y = 2 };\n  };\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::type_mismatch"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result.stderr.contains("`Point` has no field `y`"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_duplicate_comptime_struct_field() {
    let result = compile(
        "comptime_struct_duplicate_field",
        "comptime struct Point {\n  x: comptime_int,\n}\n\nfn main() {\n  comptime {\n    let point = Point { .x = 1, .x = 2 };\n  };\n}\n",
    );

    assert!(!result.success);
    assert!(
        result
            .stderr
            .contains("duplicate field `x` in comptime struct value"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_missing_comptime_struct_field() {
    let result = compile(
        "comptime_struct_missing_field",
        "comptime struct Point {\n  x: comptime_int,\n  y: comptime_int,\n}\n\nfn main() {\n  comptime {\n    let point = Point { .x = 1 };\n  };\n}\n",
    );

    assert!(!result.success);
    assert!(
        result
            .stderr
            .contains("missing field `y` in comptime struct value `Point`"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_runtime_type_in_comptime_struct() {
    let result = compile(
        "comptime_struct_runtime_field",
        "comptime struct Point {\n  x: i32,\n}\n\nfn main() {}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::runtime_type"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result.stderr.contains(
            "runtime type `i32` is not allowed in comptime code, use `comptime_int` instead"
        ),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_comptime_struct_in_runtime_code() {
    let result = compile(
        "comptime_struct_runtime_value",
        "comptime struct Point {\n  x: comptime_int,\n}\n\ncomptime Value = Point { .x = 1 };\n\nfn main() {\n  @println(\"{}\", Value);\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::runtime_only"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("comptime Point values cannot be used in runtime code"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_unknown_comptime_struct_method() {
    let result = compile(
        "comptime_struct_unknown_method",
        "comptime struct Point {\n  x: comptime_int,\n}\n\nfn main() {\n  comptime {\n    let point = Point { .x = 1 };\n    point.missing();\n  };\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("`Point` has no method `missing`"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_generic_comptime_struct() {
    let result = compile(
        "comptime_struct_generic",
        "comptime struct Point[T] {\n  value: T,\n}\n\nfn main() {}\n",
    );

    assert!(!result.success);
    assert!(
        result
            .stderr
            .contains("comptime structs cannot have generic parameters"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn imports_public_comptime_struct() {
    let files = [
        (
            "math.zn",
            "pub comptime struct Point {\n  x: comptime_int,\n  y: comptime_int,\n\n  fn sum(self) comptime_int {\n    return self.x + self.y;\n  }\n}\n\ncomptime struct Secret {\n  value: comptime_int,\n}\n\npub comptime fn make(x: comptime_int) Point {\n  return Point { .x = x, .y = x };\n}\n",
        ),
        (
            "main.zn",
            "use math;\n\ncomptime fn total(point: Point) comptime_int {\n  return point.sum();\n}\n\nfn main() {\n  comptime {\n    let point = make(4);\n    @println(\"{} {}\", point.sum(), total(Point { .x = 7, .y = 2 }));\n  };\n}\n",
        ),
    ];

    let result = compile_files("comptime_struct_module", &files);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("[comptime][main.zn:8] 8 9"),
        "stdout: {}",
        result.stdout
    );
}

#[test]
fn rejects_private_comptime_struct_across_modules() {
    let files = [
        (
            "math.zn",
            "comptime struct Secret {\n  value: comptime_int,\n}\n",
        ),
        (
            "main.zn",
            "use math;\n\nfn main() {\n  comptime {\n    let secret = Secret { .value = 1 };\n  };\n}\n",
        ),
    ];

    let result = compile_files("comptime_struct_private", &files);

    assert!(!result.success);
    assert!(
        result
            .stderr
            .contains("comptime struct `Secret` is not public in the module that defines it"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn inserts_captured_expressions() {
    let source = r#"comptime fn twice(n: comptime_int) zeen_expr {
  return @zeen_expr($(n) + $(n));
}

comptime Captured = @zeen_expr(1 + 2);

fn main() {
  let value = $(Captured);
  let four = $(twice(2));
  @println("{} {}", value, four);
}
"#;

    let result = run("quote_expr", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(result.stdout.contains("3 4"), "stdout: {}", result.stdout);
}

#[test]
fn captures_expressions_without_evaluation() {
    let source = r#"comptime fn total(x: comptime_int) zeen_expr {
  return @zeen_expr($(x) + @zeen_expr(1));
}

fn main() {
  let value = $(total(1));
  @println("{}", value);
}
"#;

    let result = run("quote_expr_nested", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(result.stdout.contains("2"), "stdout: {}", result.stdout);
}

#[test]
fn interpolates_strings_inside_captured_expressions() {
    let source = r#"comptime Text = @zeen_expr("x = $(1 + 2)");

fn main() {
  @println($(Text));
}
"#;

    let result = run("quote_expr_string", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(result.stdout.contains("x = 3"), "stdout: {}", result.stdout);
}

#[test]
fn substitutes_type_splices() {
    let source = r#"comptime fn sized(n: comptime_int) zeen_type {
  return @zeen_type([$(n)]i32);
}

comptime Array = sized(3);

fn main() {
  let values: Array = [1, 2, 3];
  @println("{}", values[2]);
}
"#;

    let result = run("quote_type_splice", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(result.stdout.contains("3"), "stdout: {}", result.stdout);
}

#[test]
fn requires_splice_for_type_values() {
    let result = compile(
        "quote_type_missing_splice",
        "comptime fn sized(n: comptime_int) zeen_type {\n  return @zeen_type([n]i32);\n}\n\ncomptime Array = sized(3);\n\nfn main() {\n  let values: Array = [1, 2, 3];\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::resolver::unresolved_ident"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result.stderr.contains("unresolved identifier: n"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_decl_values_in_runtime_code() {
    let result = compile(
        "quote_decl_runtime",
        "comptime fn make() zeen_decl {\n  return @zeen_decl(struct Foo { x: i32 });\n}\n\nfn main() {\n  let value = make();\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::runtime_only"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("comptime zeen_decl values can only be used with @emit"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_non_capture_value_in_splice() {
    let result = compile(
        "quote_splice_mismatch",
        "fn main() {\n  let x = $(\"text\");\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::type_mismatch"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("`$()` in runtime code expects a zeen_expr, got comptime_str"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn reports_inserted_expressions_at_call_site() {
    let source = r#"comptime fn bad() zeen_expr {
  return @zeen_expr(1);
}

fn main() {
  let x: bool = $(bad());
}
"#;

    let result = compile("quote_expr_span", source);

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::typechecker::mismatch"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result.stderr.contains("main.zn:6:17"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn emits_declarations_from_comptime() {
    let source = r#"comptime fn named(name: comptime_str) zeen_decl {
  @zeen_decl(
    pub struct $(name) {
      pub value: i32
    }
  );
}

comptime fn adder() zeen_decl {
  @zeen_decl(
    pub fn inc(x: i32) i32 {
      return x + 1;
    }
  );
}

comptime {
  @emit(named("Point"));
  @emit(adder());
}

fn main() {
  let point = Point { .value = 41 };
  @println("{}", inc(point.value));
}
"#;

    let result = run("emit_declarations", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(result.stdout.contains("42"), "stdout: {}", result.stdout);
}

#[test]
fn emits_declarations_in_a_loop_with_fresh_names() {
    let source = r#"comptime {
  for (i : 0..3) {
    let name = @fresh_name("Num");

    @emit(@zeen_decl(
      pub struct $(name) {
        pub value: i32
      }
    ));
  }
}

fn main() {
  let first = Num_0 { .value = 1 };
  let second = Num_1 { .value = 2 };
  let third = Num_2 { .value = 3 };
  @println("{}", first.value + second.value + third.value);
}
"#;

    let result = run("emit_loop", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(result.stdout.contains("6"), "stdout: {}", result.stdout);
}

#[test]
fn rejects_emit_without_declaration() {
    let result = compile(
        "emit_mismatch",
        "comptime {\n  @emit(1);\n}\n\nfn main() {}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::type_mismatch"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("@emit expects a zeen_decl, got comptime_int"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_name_splice_without_string() {
    let result = compile(
        "emit_name_mismatch",
        "comptime {\n  @emit(@zeen_decl(pub struct $(1) { pub value: i32 }));\n}\n\nfn main() {}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::type_mismatch"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("a declaration name splice expects a comptime_str, got comptime_int"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_duplicate_emitted_name() {
    let result = compile(
        "emit_duplicate",
        "comptime {\n  @emit(@zeen_decl(pub struct Twin { pub value: i32 }));\n  @emit(@zeen_decl(pub struct Twin { pub other: i32 }));\n}\n\nfn main() {}\n",
    );

    assert!(!result.success);
    assert!(
        result
            .stderr
            .contains("zeen::comptime::duplicate_definition"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result.stderr.contains("duplicate definition of `Twin`"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_emit_in_runtime_code() {
    let result = compile(
        "emit_runtime",
        "comptime Value = @zeen_decl(pub struct Late { pub value: i32 });\n\nfn main() {\n  @emit(Value);\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::runtime_only"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("`@emit` is only available in comptime code"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_fresh_name_in_runtime_code() {
    let result = compile(
        "fresh_name_runtime",
        "fn main() {\n  let name = @fresh_name(\"Num\");\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::runtime_only"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("`@fresh_name` is only available in comptime code"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn emits_from_nested_comptime_block() {
    let source = r#"fn prepare() void {
  comptime {
    @emit(@zeen_decl(
      pub struct Inner {
        pub value: i32
      }
    ));
  }
}

fn main() {
  let inner = Inner { .value = 7 };
  @println("{}", inner.value);
}
"#;

    let result = run("emit_nested_block", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(result.stdout.contains("7"), "stdout: {}", result.stdout);
}

fn compile_target(name: &str, target: &str, source: &str) -> CompileResult {
    let dir = std::env::temp_dir().join(format!(
        "zeen_comptime_target_{}_{name}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let path = dir.join("main.zn");
    fs::write(&path, source).unwrap();

    let output: Output = Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .arg(&path)
        .arg("--check")
        .arg("--target")
        .arg(target)
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
fn compares_sizeof_with_runtime_layout() {
    let source = r#"struct Point {
  x: i32,
  y: i32,
}

struct Mixed {
  tag: i8,
  point: Point,
  count: i64,
}

enum Shape {
  empty,
  circle: Point,
  index: i64,
}

comptime POINT = @sizeof(Point);
comptime MIXED = @sizeof(Mixed);
comptime SHAPE = @sizeof(Shape);
comptime ALIGN = @alignof(Shape);

fn main() {
  @println("{} {} {} {}", POINT == @sizeof(Point), MIXED == @sizeof(Mixed), SHAPE == @sizeof(Shape), ALIGN == @alignof(Shape));
  @println("{} {} {}", POINT, MIXED, SHAPE);
}
"#;

    let result = run("sizeof_runtime", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("true true true true"),
        "stdout: {}",
        result.stdout
    );
    assert!(
        result.stdout.contains("8 24 16"),
        "stdout: {}",
        result.stdout
    );
}

#[test]
fn resolves_sizeof_for_imported_types() {
    let shapes = r#"pub alias Level = i16;

pub struct Vec2 {
  x: f32,
  y: f32,
}

pub enum Slot {
  empty,
  item: Vec2,
  index: i32,
}
"#;

    let main = r#"use shapes;

comptime {
  @println("{} {} {}", @sizeof(Vec2), @sizeof(Slot), @alignof(Level));
}

fn main() {}
"#;

    let result = compile_files(
        "sizeof_modules",
        &[("main.zn", main), ("shapes.zn", shapes)],
    );

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("[comptime][main.zn:3] 8 12 2"),
        "stdout: {}",
        result.stdout
    );
}

#[test]
fn sizeof_follows_the_target_pointer_width() {
    let source = r#"comptime {
  @println("{} {} {}", @sizeof(*i32), @sizeof(usize), @sizeof([2]*i32));
}

fn main() {}
"#;

    let wide = compile_target("sizeof_target_wide", "x86_64-unknown-linux-gnu", source);
    assert!(wide.success, "stderr: {}", wide.stderr);
    assert!(wide.stdout.contains("8 8 16"), "stdout: {}", wide.stdout);

    let narrow = compile_target("sizeof_target_narrow", "wasm32-unknown-unknown", source);
    assert!(narrow.success, "stderr: {}", narrow.stderr);
    assert!(narrow.stdout.contains("4 4 8"), "stdout: {}", narrow.stdout);
}

#[test]
fn rejects_sizeof_of_unknown_type() {
    let result = compile(
        "sizeof_unknown",
        "comptime SIZE = @sizeof(Missing);\n\nfn main() {\n  @println(\"{}\", SIZE);\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::unresolved_ident"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_sizeof_of_generic_type_without_arguments() {
    let result = compile(
        "sizeof_generic",
        "struct Box[T] {\n  value: T\n}\n\ncomptime SIZE = @sizeof(Box);\n\nfn main() {\n  @println(\"{}\", SIZE);\n}\n",
    );

    assert!(!result.success);
    assert!(
        result.stderr.contains("zeen::comptime::type_mismatch"),
        "stderr: {}",
        result.stderr
    );
    assert!(
        result
            .stderr
            .contains("@sizeof requires generic arguments for Box"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_sizeof_without_type() {
    let result = compile(
        "sizeof_no_arg",
        "comptime SIZE = @sizeof();\n\nfn main() {\n  @println(\"{}\", SIZE);\n}\n",
    );

    assert!(!result.success);
}

#[test]
fn computes_sizeof_for_generic_types() {
    let source = r#"struct Inner[T] {
  value: T,
}

struct Outer[T] {
  inner: Inner[T],
  marker: T,
}

enum Maybe[T] {
  none,
  some: T,
}

alias Pointer[T] = *T;

comptime fn total() comptime_int {
  return @sizeof(Outer[i64]) + @sizeof(Maybe[i32]) + @sizeof(Pointer[i16]);
}

fn main() {
  @println("{} {} {}", @sizeof(Outer[i64]), @sizeof(Maybe[i32]), @sizeof(Pointer[i16]));
  @println("{}", total());
}
"#;

    let result = run("sizeof_generics", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        result.stdout.contains("16 8 8"),
        "stdout: {}",
        result.stdout
    );
    assert!(result.stdout.contains("32"), "stdout: {}", result.stdout);
}

#[test]
fn reuses_cached_results_of_identical_calls() {
    let source = r#"comptime fn sum(n: comptime_int) comptime_int {
  let acc: comptime_int = 0;
  for (i : 0..n) {
    acc += i;
  }
  return acc;
}

fn main() {
  comptime {
    let a = sum(20000);
    let b = sum(20000);
    let c = sum(20000);
    let d = sum(20000);
  };
}
"#;

    let result = compile("cache_calls", source);

    assert!(result.success, "stderr: {}", result.stderr);
}

#[test]
fn recomputes_calls_with_distinct_arguments() {
    let source = r#"comptime fn sum(n: comptime_int) comptime_int {
  let acc: comptime_int = 0;
  for (i : 0..n) {
    acc += i;
  }
  return acc;
}

fn main() {
  comptime {
    let a = sum(20000);
    let b = sum(20001);
    let c = sum(20002);
    let d = sum(20003);
  };
}
"#;

    let result = compile("cache_args", source);

    assert!(!result.success);
    assert!(
        result.stderr.contains("comptime step limit exceeded"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_emit_of_comptime_declaration() {
    let result = compile(
        "emit_comptime_decl",
        "comptime {\n  @emit(@zeen_decl(comptime X = 1;));\n}\n\nfn main() {\n}\n",
    );

    assert!(!result.success);
    assert!(
        result
            .stderr
            .contains("@emit cannot emit a comptime constant declaration"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn rejects_emit_above_declaration_limit() {
    let source = r#"comptime {
  for (i : 0..1025) {
    let name = @fresh_name("Bulk");

    @emit(@zeen_decl(
      pub struct $(name) {
        pub index: i32
      }
    ));
  }
}

fn main() {
}
"#;

    let result = compile("emit_limit", source);

    assert!(!result.success);
    assert!(
        result.stderr.contains("@emit exceeded the limit"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn void_comptime_calls_do_not_warn() {
    let source = r#"comptime fn spell() void {
  @println("compile time");
}

fn main() {
  spell();
  @println("run time");
}
"#;

    let result = compile("void_call", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        !result.stderr.contains("unused result of expression"),
        "stderr: {}",
        result.stderr
    );
}

#[test]
fn untyped_comptime_calls_do_not_warn() {
    let source = r#"comptime fn note() {
  @println("compile time");
}

fn main() {
  note();
  @println("run time");
}
"#;

    let result = compile("untyped_call", source);

    assert!(result.success, "stderr: {}", result.stderr);
    assert!(
        !result.stderr.contains("unused result of expression"),
        "stderr: {}",
        result.stderr
    );
}
