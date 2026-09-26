use miette::Diagnostic;
use thiserror::Error;

#[derive(Debug, Error, Diagnostic)]
pub enum CodegenError {
    #[error("unsupported target triple `{triple}`: {detail}")]
    #[diagnostic(code(zeen::codegen::unsupported_triple))]
    UnsupportedTriple { triple: String, detail: String },

    #[error("LLVM failed to verify the generated module `{module}`")]
    #[diagnostic(
        code(zeen::codegen::verify_failed),
        url("https://github.com/mealet/zeen/issues/new"),
        help("This is a compiler bug. Please report it with the full output.")
    )]
    ModuleVerificationFailed { module: String, detail: String },

    #[error("LLVM pass pipeline failed: {detail}")]
    #[diagnostic(code(zeen::codegen::pass_pipeline_failed))]
    PassPipelineFailed { detail: String },

    #[error("failed to write {kind} output to `{path}`: {detail}")]
    #[diagnostic(code(zeen::codegen::emit_failed))]
    EmitFailed {
        kind: &'static str,
        path: String,
        detail: String,
    },
}
