use smol_str::SmolStr;
use std::{path::PathBuf, sync::Arc};
use zeen_resolve::DefId;

use miette::{Diagnostic, NamedSource, SourceSpan};
use thiserror::Error;

use zeen_ast::expressions::{BinaryOp, UnaryOp};

// TODO: Remove `#![allow(unused)]` and delete unused code blocks from codebase when finished TypeChecker

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

    #[error("unable to infer generic type")]
    #[diagnostic(severity(Error), code(zeen::typechecker::cannot_infer_generic))]
    CannotInferGeneric {
        #[source_code]
        src: NamedSource<Arc<String>>,

        #[label(primary, "infer requested here")]
        span: SourceSpan,

        #[related]
        declared: Vec<InferGenericDeclared>,
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

    #[error("usage of break outside loop")]
    #[diagnostic(severity(Error), code(zeen::typechecker::break_outside_loop))]
    BreakOutsideLoop {
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

    #[error("array length must be constant")]
    #[diagnostic(severity(Error), code(zeen::typechecker::array_length_not_const))]
    ArrayLengthNotConst {
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
    InterfaceNotAvaible {
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
    #[diagnostic(severity(Error), code(zeen::typechecker::interface_signature_mismatch))]
    InterfaceMethodSignatureMismatch {
        interface: SmolStr,
        method: SmolStr,

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

#[derive(Debug, Error, Diagnostic, Clone)]
#[error("type declared here")]
#[diagnostic(severity(Advice))]
pub struct InferGenericDeclared {
    #[source_code]
    pub src: NamedSource<Arc<String>>,

    #[label]
    pub span: SourceSpan,
}
