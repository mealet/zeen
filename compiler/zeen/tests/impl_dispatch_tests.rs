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

/// Ordering operators exist only via `Ord`; a struct with just `Eq` rejects
/// them like any unsupported binary operator.
#[test]
fn ordering_op_needs_ord() {
    compile_fails(
        "ordering_needs_ord",
        r#"
struct Foo {
  pub x: i32
}

implement Eq : Foo {
  fn eq(*const self, other: *const Self) bool {
    self.x == other.x
  }
}

fn main() {
  let a = Foo { .x = 1 };
  let b = Foo { .x = 2 };
  @println("{}", a < b);
}
"#,
        "zeen::typechecker::not_supported_binary",
    );
}

/// `Ord.cmp` takes `*const Self`; a mixed comparison with a non-struct value
/// is a mismatched argument, not a valid ordering.
#[test]
fn ordering_op_type_mismatch() {
    compile_fails(
        "ordering_type_mismatch",
        r#"
struct Foo {
  pub x: i32
}

implement Ord : Foo {
  fn cmp(*const self, other: *const Self) Ordering {
    if (self.x < other.x) {
      Ordering.Less
    } else if (self.x > other.x) {
      Ordering.Greater
    } else {
      Ordering.Equal
    }
  }
}

fn main() {
  let a = Foo { .x = 1 };
  @println("{}", a < 5);
}
"#,
        "zeen::typechecker::mismatch",
    );
}

/// An ordering operator on an unbounded generic parameter reports a missing
/// `Ord` bound instead of silently comparing the representation.
#[test]
fn ordering_op_on_unbounded_generic() {
    compile_fails(
        "ordering_unbounded_generic",
        r#"
fn less_than[T](a: T, b: T) bool {
  return a < b;
}

fn main() {
  @println("{}", less_than(1, 2));
}
"#,
        "zeen::typechecker::generic_missing_bound",
    );
}

/// An `Eq` bound does not license ordering operators.
#[test]
fn ordering_op_on_eq_only_generic() {
    compile_fails(
        "ordering_eq_only_generic",
        r#"
fn less_than[T: Eq](a: T, b: T) bool {
  return a > b;
}

fn main() {
  @println("{}", less_than(1, 2));
}
"#,
        "zeen::typechecker::generic_missing_bound",
    );
}

/// An equality operator on an unbounded generic parameter reports a missing
/// `Eq` bound instead of comparing the representation.
#[test]
fn equality_op_on_unbounded_generic() {
    compile_fails(
        "equality_unbounded_generic",
        r#"
fn same[T](a: T, b: T) bool {
  return a == b;
}

fn main() {
  @println("{}", same(1, 2));
}
"#,
        "zeen::typechecker::generic_missing_bound",
    );
}

/// An `Ord` bound does not license arithmetic operators.
#[test]
fn arithmetic_op_on_ord_only_generic() {
    compile_fails(
        "arithmetic_ord_only_generic",
        r#"
fn plus[T: Ord](a: T, b: T) T {
  return a + b;
}

fn main() {
  @println("{}", plus(1, 2));
}
"#,
        "zeen::typechecker::generic_missing_bound",
    );
}

/// A unary operator on an unbounded generic parameter reports a missing
/// bound instead of applying the raw operation.
#[test]
fn unary_op_on_unbounded_generic() {
    compile_fails(
        "unary_unbounded_generic",
        r#"
fn minus[T](a: T) T {
  return -a;
}

fn main() {
  @println("{}", minus(5));
}
"#,
        "zeen::typechecker::generic_missing_bound",
    );
}

/// A method generic repeating the struct generic adds a bound to it: the
/// method only resolves when the struct argument satisfies the bound.
#[test]
fn method_bound_addition_rejects_unsatisfied() {
    compile_fails(
        "method_bound_addition",
        r#"
struct Wrap[T] {
  inner: T,

  pub fn dup_inner[T: Clone](*const self) T {
    return self.inner.clone();
  }
}

fn main() {
  let w = Wrap { .inner = 5 };
  let q = w.dup_inner();
}
"#,
        "zeen::typechecker::generic_bound_not_satisfied",
    );
}

/// A direct call to a bounded implement-block method checks the block
/// bounds: a method from `implement[T: Copy]` does not apply to non-`Copy`
/// instantiations.
#[test]
fn bounded_impl_method_rejects_unsatisfied() {
    compile_fails(
        "bounded_impl_method",
        r#"
struct Wrap[T] {
  pub inner: T,
}

struct Foo {
  pub x: i32,
}

interface Check {
  fn check(*const self) bool;
}

implement[T: Copy] Check : Wrap[T] {
  fn check(*const self) bool {
    return true;
  }
}

fn main() {
  let w = Wrap { .inner = Foo { .x = 1 } };
  @println("{}", w.check());
}
"#,
        "zeen::typechecker::generic_bound_not_satisfied",
    );
}
