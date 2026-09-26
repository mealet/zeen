use std::{cell::RefCell, collections::HashMap, path::Path, rc::Rc};

use inkwell::{
    AddressSpace, FloatPredicate, IntPredicate, OptimizationLevel,
    basic_block::BasicBlock,
    builder::Builder,
    context::Context,
    module::Module,
    targets::{
        CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetData, TargetMachine,
        TargetTriple,
    },
    types::{AnyTypeEnum, BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType},
    values::{
        BasicMetadataValueEnum, BasicValueEnum, FunctionValue, GlobalValue, IntValue, PointerValue,
        ValueKind,
    },
};
use lasso::{Rodeo, Spur};
use zeen_ast::{
    expressions::{BinaryOp, UnaryOp},
    types::BuiltinType,
};
use zeen_driver::CompilationMode;
use zeen_hir::HirMacroKind;
use zeen_mir::{
    BlockId, CallTarget, ConstValue, LocalId, MirFunction, MirFunctionId, MirProgram, MirStatement,
    Operand, Place, PlaceElem, Rvalue, Terminator,
};
use zeen_resolve::{DefId, ResolutionResult};
use zeen_typecheck::{
    coerce::{builtin_is_float, builtin_is_integer},
    format_str::{FormatChunk, FormatSpec},
    result::TypeCheckResult,
};
use zeen_types::{Type, TypeId};

use crate::error::CodegenError;

/// Compilation options for the codegen stage.
#[derive(Debug, Clone)]
pub struct CodegenOptions {
    /// Debug vs Release (affects optimization level and panic strategy).
    pub mode: CompilationMode,
    /// User-provided target triple (see `--target`). `None` = host triple.
    pub target: Option<String>,
    /// The real `main` function of the program, if any. It is emitted under
    /// the symbol `zeen_main`, wrapped by a generated `main` entry point.
    pub main_fn: Option<MirFunctionId>,
    /// Source file name, used for the module's `source_filename` metadata.
    pub source_file_name: String,
}

impl Default for CodegenOptions {
    fn default() -> Self {
        Self {
            mode: CompilationMode::Debug,
            target: None,
            main_fn: None,
            source_file_name: "test.zn".to_string(),
        }
    }
}

pub struct CodeGen<'ctx, 'prog> {
    context: &'ctx Context,
    builder: Builder<'ctx>,
    module: Module<'ctx>,
    machine: TargetMachine,
    target_data: TargetData,

    program: &'prog MirProgram,
    typecheck: &'prog TypeCheckResult,
    resolution: &'prog ResolutionResult,
    rodeo: Rc<RefCell<Rodeo>>,

    options: CodegenOptions,

    // module-level caches
    functions: HashMap<MirFunctionId, FunctionValue<'ctx>>,
    struct_types: HashMap<TypeId, inkwell::types::StructType<'ctx>>,
    strings: HashMap<String, GlobalValue<'ctx>>,
    str_counter: u32,

    // per-function state
    locals: HashMap<LocalId, PointerValue<'ctx>>,
    blocks: HashMap<BlockId, BasicBlock<'ctx>>,
}

