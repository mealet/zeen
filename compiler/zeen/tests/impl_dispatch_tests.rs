use assert_cmd::Command;

/// Compiles `source` and asserts that the compiler rejects it reporting
/// `expected_code` in its diagnostics.
fn compile_fails(name: &str, source: &str, expected_code: &str) {
    let dir = std::env::temp_dir().join(format!(
        "zeen_impl_dispatch_{}_{}",
        name,
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let src_path = dir.join(format!("{name}.zn"));
    std::fs::write(&src_path, source).unwrap();
    let out_path = dir.join(format!("{name}.bin"));

    let output = Command::cargo_bin(env!("CARGO_PKG_NAME"))
        .unwrap()
        .arg(&src_path)
        .arg(&out_path)
        .assert()
        .failure();

    let stderr = String::from_utf8_lossy(&output.get_output().stderr);
    assert!(
        stderr.contains(expected_code),
        "expected `{expected_code}` in diagnostics:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A generic implementation applies only to type arguments satisfying its
/// bounds: `Foo` implements nothing, so `Val[Foo]` must not resolve to it.
#[test]
fn generic_impl_bound_not_satisfied() {
    compile_fails(
        "generic_bound",
        r#"
struct Val[T] {
  inner: T,

  pub fn new(value: T) Self {
    Self { .inner = value }
  }
}

implement[T: Display] Display : Val[T] {
  fn display(*const self, out: OutStream) void {
    out.write_str("val");
  }
}

struct Foo {}

fn main() {
  let a = Val.new(Foo {});
  @println("{}", a);
}
"#,
        "zeen::typechecker::interface_not_implemented",
    );
}

/// A specialization is picked only for its exact instantiation; other
/// instantiations must not silently fall back to it.
#[test]
fn specialization_miss_reports_error() {
    compile_fails(
        "spec_miss",
        r#"
struct Val[T] {
  inner: T,

  pub fn new(value: T) Self {
    Self { .inner = value }
  }
}

implement Display : Val[i32] {
  fn display(*const self, out: OutStream) void {
    out.write_str("spec");
  }
}

struct Foo {}

fn main() {
  let a = Val.new(Foo {});
  @println("{}", a);
}
"#,
        "zeen::typechecker::interface_not_implemented",
    );
}

/// Bound satisfaction checks the type argument recursively: `Val[Foo]` does
/// not satisfy `Display`, so neither does `Val[Val[Foo]]`.
#[test]
fn nested_generic_bound_not_satisfied() {
    compile_fails(
        "nested_bound",
        r#"
struct Val[T] {
  inner: T,

  pub fn new(value: T) Self {
    Self { .inner = value }
  }
}

implement[T: Display] Display : Val[T] {
  fn display(*const self, out: OutStream) void {
    out.write_str("val");
  }
}

struct Foo {}

fn main() {
  let a = Val.new(Val.new(Foo {}));
  @println("{}", a);
}
"#,
        "zeen::typechecker::interface_not_implemented",
    );
}

/// Every bound of the implementation must be satisfied: `HasDisplay`
/// implements `Display` but not `Eq`.
#[test]
fn multi_bound_missing_interface() {
    compile_fails(
        "multi_bound",
        r#"
struct Val[T] {
  inner: T,

  pub fn new(value: T) Self {
    Self { .inner = value }
  }
}

implement[T: Display + Eq] Display : Val[T] {
  fn display(*const self, out: OutStream) void {
    out.write_str("val");
  }
}

struct HasDisplay {}

implement Display : HasDisplay {
  fn display(*const self, out: OutStream) void {
    out.write_str("has-display");
  }
}

fn main() {
  let a = Val.new(HasDisplay {});
  @println("{}", a);
}
"#,
        "zeen::typechecker::interface_not_implemented",
    );
}

/// Implementing the same interface for the same instantiation twice is
/// rejected: `Foo` already implements `Add` by the second block.
#[test]
fn duplicate_impl_reported() {
    compile_fails(
        "duplicate_impl",
        r#"
struct Foo {
  pub x: i32
}

implement Add : Foo {
  fn add(self, other: Self) Self {
    Self { .x = self.x + other.x }
  }
}

implement Add : Foo {
  fn add(self, other: Self) Self {
    Self { .x = self.x + other.x }
  }
}

fn main() {
  let a = Foo { .x = 10 };
  let b = Foo { .x = 5 };
  @println("{}", (a + b).x);
}
"#,
        "zeen::typechecker::duplicate_impl",
    );
}

/// A generic implementation coexists with a specialization, but two
/// specializations of the same instantiation are still a duplicate.
#[test]
fn duplicate_specialization_reported() {
    compile_fails(
        "generic_plus_spec",
        r#"
struct Val[T] {
  inner: T,

  pub fn new(value: T) Self {
    Self { .inner = value }
  }
}

implement[T: Display] Display : Val[T] {
  fn display(*const self, out: OutStream) void {
    out.write_str("GENERIC");
  }
}

implement Display : Val[i32] {
  fn display(*const self, out: OutStream) void {
    out.write_str("SPEC-I32");
  }
}

implement Display : Val[i32] {
  fn display(*const self, out: OutStream) void {
    out.write_str("SPEC-I32-DUP");
  }
}

fn main() {
  let a = Val.new(42);
  @println("{}", a);
}
"#,
        "zeen::typechecker::duplicate_impl",
    );
}

/// The implementing method's signature must match the interface one after
/// substituting the struct's generics: `Deref::deref` returns `T`, not `i32`.
#[test]
fn wrong_method_signature_reported() {
    compile_fails(
        "wrong_sig",
        r#"
struct Holder[T] {
  value: T,

  pub fn new(value: T) Self {
    Self { .value = value }
  }
}

implement[T] Deref : Holder[T] {
  fn deref(*const self) i32 {
    0
  }
}

fn main() {}
"#,
        "zeen::typechecker::interface_signature_mismatch",
    );
}
