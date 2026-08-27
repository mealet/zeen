use smol_str::SmolStr;
use std::sync::Arc;

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

use zeen_ast::expressions::{BinaryOp, UnaryOp};

#[derive(Debug, Error, Diagnostic, Clone)]
pub enum TypeError {
    #[error("expected type `{expected}`, but found `{found}`")]
    #[diagnostic(severity(Error), code(zeen::typechecker::mismatch))]
    Mismatch {
        expected: SmolStr,
        found: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("entry point `fn main() {{ ... }}` not found")]
    #[diagnostic(
        severity(Error),
        code(zeen::typechecker::main_not_found),
        help("add function declaration with signature: `fn main() any {{ ... }}`")
    )]
    MainNotFound {
        #[source_code]
        src: NamedSource<Arc<String>>,
    },

    #[error("main function signature mismatch")]
    #[diagnostic(
        severity(Error),
        code(zeen::typechecker::main_signature_mismatch),
        help("consider using right signature: `fn main() any {{ ... }}`")
    )]
    MainSignatureMismatch {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("nested functions cannot be `public`")]
    #[diagnostic(
        severity(Error),
        code(zeen::typechecker::nested_fn_pub),
        help("remove the `pub` modifier: nested functions are only visible from their parent")
    )]
    NestedFnPub {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("invalid variadic arguments in function")]
    #[diagnostic(
        severity(Error),
        code(zeen::typechecker::invalid_va_args),
        help("variadic args must be last argument: `fn foo(arg: type, ...)`")
    )]
    InvalidVaArgs {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("variadic args usage on non-extern functions")]
    #[diagnostic(severity(Error), code(zeen::typechecker::non_extern_va_args))]
    NonExternVaArgs {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("unknown field in '{struct_name}': `{field}`")]
    #[diagnostic(severity(Error), code(zeen::typechecker::unknown_field))]
    UnknownField {
        struct_name: SmolStr,
        field: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("missing fields in '{struct_name}': {fields}")]
    #[diagnostic(severity(Error), code(zeen::typechecker::missing_fields))]
    MissingFields {
        struct_name: SmolStr,
        fields: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("provided `{provided}` is not a struct")]
    #[diagnostic(severity(Error), code(zeen::typechecker::not_a_struct))]
    NotAStruct {
        provided: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("type `{ty}` is not callable")]
    #[diagnostic(severity(Error), code(zeen::typechecker::not_callable))]
    NotCallable {
        ty: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("expected {expected} args, but found {found}")]
    #[diagnostic(severity(Error), code(zeen::typechecker::arg_count_mismatch))]
    ArgCountMismatch {
        expected: usize,
        found: usize,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("invalid type cast: `{from}` -> `{to}`")]
    #[diagnostic(severity(Error), code(zeen::typechecker::arg_count_mismatch))]
    InvalidCast {
        from: SmolStr,
        to: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("cannot implement `Drop` for `{struct_name}`: the struct also implements `Copy`")]
    #[diagnostic(
        severity(Error),
        code(zeen::typechecker::copy_with_drop),
        help("`Copy` types own no resources to release; pick one: `Copy` or `Drop`")
    )]
    CopyWithDrop {
        struct_name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("`{method}` is a static method and cannot be called on an instance")]
    #[diagnostic(
        severity(Error),
        code(zeen::typechecker::static_method_on_instance),
        help("use `Type.method(...)` to call a static method")
    )]
    StaticMethodOnInstance {
        method: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("cannot infer `{generic_name}` generic type")]
    #[diagnostic(severity(Error), code(zeen::typechecker::cannot_infer_generic))]
    CannotInferGeneric {
        generic_name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,

        #[label(primary, "infer requested here")]
        span: SourceSpan,
    },

    #[error("binary '{op}' is not supported between: `{lhs_type}` and `{rhs_type}`")]
    #[diagnostic(severity(Error), code(zeen::typechecker::not_supported_binary))]
    BinaryNotSupported {
        op: BinaryOp,
        lhs_type: SmolStr,
        rhs_type: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("unary '{op}' is not supported for: `{child_type}`")]
    #[diagnostic(severity(Error), code(zeen::typechecker::not_supported_unary))]
    UnaryNotSupported {
        op: UnaryOp,
        child_type: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("usage of `break` outside loop")]
    #[diagnostic(severity(Error), code(zeen::typechecker::break_outside_loop))]
    BreakOutsideLoop {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("usage of `continue` outside loop")]
    #[diagnostic(severity(Error), code(zeen::typechecker::continue_outside_loop))]
    ContinueOutsideLoop {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("type `{child_type}` is not indexable")]
    #[diagnostic(severity(Error), code(zeen::typechecker::not_indexable))]
    NotIndexable {
        child_type: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("type `{child_type}` is not iterable")]
    #[diagnostic(severity(Error), code(zeen::typechecker::not_indexable))]
    NotIterable {
        child_type: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("array length must be constant")]
    #[diagnostic(severity(Error), code(zeen::typechecker::array_length_not_const))]
    ArrayLengthNotConst {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("empty arrays are not allowed")]
    #[diagnostic(severity(Error), code(zeen::typechecker::empty_array))]
    EmptyArrayError {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("array length overflows the supported range")]
    #[diagnostic(severity(Error), code(zeen::typechecker::array_length_overflow))]
    ArrayLengthOverflow {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("repeat array init requires a Copy element type")]
    #[diagnostic(severity(Error), code(zeen::typechecker::repeat_init_not_copy))]
    RepeatInitNotCopy {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("attempt to assign to const")]
    #[diagnostic(severity(Error), code(zeen::typechecker::assign_to_const))]
    AssignToConst {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("found dangling definition id: DefId({id})")]
    #[diagnostic(
        severity(Error),
        code(zeen::typechecker::dangling_defid),
        help("please report this on: https://github.com/mealet/zeen/issues")
    )]
    DanglingDefId {
        id: u32,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("interface `{name}` is not avaible here")]
    #[diagnostic(
        severity(Error),
        code(zeen::typechecker::interface_not_avaible),
        help("try to import 'ops' module from standard library: `use std.ops`")
    )]
    InterfaceNotAvailable {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("unknown macro found")]
    #[diagnostic(severity(Error), code(zeen::typechecker::unknown_macro))]
    UnknownMacro {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("type `{ty_name}` has no implementation for `{name}` interface")]
    #[diagnostic(severity(Error), code(zeen::typechecker::interface_not_implemented))]
    InterfaceNotImplemented {
        name: SmolStr,
        ty_name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("signature mismatch in: `{interface}` -> `{method}`")]
    #[diagnostic(
        severity(Error),
        code(zeen::typechecker::interface_signature_mismatch),
        help("expected signature: `{signature}`")
    )]
    InterfaceMethodSignatureMismatch {
        interface: SmolStr,
        method: SmolStr,
        signature: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("method `{method}` in interface `{interface}` is missing")]
    #[diagnostic(severity(Error), code(zeen::typechecker::interface_method_missing))]
    InterfaceMethodMissing {
        interface: SmolStr,
        method: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("operator interface `{operator}` is not supported on provided generic")]
    #[diagnostic(severity(Error), code(zeen::typechecker::generic_op_not_supported))]
    OperatorNotSupportedOnGeneric {
        operator: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("type `{name}` expected {expected} generic types, but found {found}")]
    #[diagnostic(severity(Error), code(zeen::typechecker::generic_count_mismatch))]
    GenericArgCountMismatch {
        name: SmolStr,
        expected: usize,
        found: usize,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("generic `{generic}` missing interface bound: `{bound}`")]
    #[diagnostic(severity(Error), code(zeen::typechecker::generic_missing_bound))]
    GenericMissingBound {
        generic: SmolStr,
        bound: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("generic `{generic}` with bound `{bound}` not satisfied with type: `{ty}`")]
    #[diagnostic(severity(Error), code(zeen::typechecker::generic_bound_not_satisfied))]
    GenericBoundNotSatisfied {
        generic: SmolStr,
        bound: SmolStr,
        ty: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("generic conflict for `{param}`: expected `{first}`, but found `{second}`")]
    #[diagnostic(severity(Error), code(zeen::typechecker::generic_conflict))]
    GenericConflict {
        param: SmolStr,
        first: SmolStr,
        second: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("cannot move value through pointer")]
    #[diagnostic(
        severity(Error),
        code(zeen::typechecker::move_through_ptr),
        help("try to dereference value from pointer: *EXPR")
    )]
    CannotMoveThroughPointer {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("implement on non-struct type found")]
    #[diagnostic(severity(Error), code(zeen::typecheck::implement_non_struct))]
    ImplementNonStruct {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("private item is not accessible: `{name}`")]
    #[diagnostic(severity(Error), code(zeen::typecheck::private_item))]
    PrivateItemNotAccessible {
        name: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("associated call on instance method found")]
    #[diagnostic(severity(Error), code(zeen::typecheck::associated_call_on_method))]
    AssociatedCallOnInstaneMethod {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("unknown '{variant}' variant for `{name}` enum")]
    #[diagnostic(severity(Error), code(zeen::typecheck::unknown_enum_variant))]
    UnknownEnumVariant {
        name: SmolStr,
        variant: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("recursive type `{ty}` is infinite")]
    #[diagnostic(
        severity(Error),
        code(zeen::typecheck::infinite_recursive_type),
        help("consider using wrapping, like pointers: `*{ty}`")
    )]
    InfiniteRecursiveType {
        ty: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    // --> Format Errors
    #[error("expected format string as argument")]
    #[diagnostic(severity(Error), code(zeen::typechecker::expected_format_str))]
    ExpectedFormatString {
        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("{message}")]
    #[diagnostic(severity(Error), code(zeen::typechecker::format_parse_error))]
    FormatParseError {
        message: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("format string provides {placeholders} placeholders, but found {args}")]
    #[diagnostic(severity(Error), code(zeen::typechecker::format_parse_error))]
    FormatArgCountMismatch {
        placeholders: usize,
        args: usize,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("format requires integer type, but found `{found}`")]
    #[diagnostic(severity(Error), code(zeen::typechecker::format_parse_error))]
    FormatRequiresInteger {
        found: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },

    #[error("format requires float type, but found `{found}`")]
    #[diagnostic(severity(Error), code(zeen::typechecker::format_parse_error))]
    FormatRequiresFloat {
        found: SmolStr,

        #[source_code]
        src: NamedSource<Arc<String>>,
        #[label]
        span: SourceSpan,
    },
    // <-- Format Errors
}
