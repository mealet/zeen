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

impl<'ctx, 'prog> CodeGen<'ctx, 'prog> {
    pub fn new(
        context: &'ctx Context,
        program: &'prog MirProgram,
        typecheck: &'prog TypeCheckResult,
        resolution: &'prog ResolutionResult,
        rodeo: Rc<RefCell<Rodeo>>,
        options: CodegenOptions,
    ) -> Result<Self, CodegenError> {
        Target::initialize_all(&InitializationConfig::default());

        let triple = match &options.target {
            Some(user) => TargetMachine::normalize_triple(&TargetTriple::create(user)),
            None => TargetMachine::get_default_triple(),
        };
        let triple =
            TargetTriple::create(&triple.as_str().to_string_lossy().replace("msvc", "gnu"));

        let target =
            Target::from_triple(&triple).map_err(|err| CodegenError::UnsupportedTriple {
                triple: triple.as_str().to_string_lossy().into_owned(),
                detail: err.to_string(),
            })?;

        let opt_level = match options.mode {
            CompilationMode::Debug => OptimizationLevel::None,
            CompilationMode::Release => OptimizationLevel::Aggressive,
        };

        let machine = target
            .create_target_machine(
                &triple,
                "generic",
                "",
                opt_level,
                RelocMode::PIC,
                CodeModel::Default,
            )
            .ok_or_else(|| CodegenError::UnsupportedTriple {
                triple: triple.as_str().to_string_lossy().into_owned(),
                detail: "target has no registered codegen backend".to_string(),
            })?;

        let target_data = machine.get_target_data();
        let data_layout = target_data.get_data_layout();

        let module = context.create_module("zeen");
        module.set_triple(&triple);
        module.set_data_layout(&data_layout);
        module.set_source_file_name(&options.source_file_name);

        let builder = context.create_builder();

        Ok(Self {
            context,
            builder,
            module,
            machine,
            target_data,
            program,
            typecheck,
            resolution,
            rodeo,
            options,
            functions: HashMap::new(),
            struct_types: HashMap::new(),
            strings: HashMap::new(),
            str_counter: 0,
            locals: HashMap::new(),
            blocks: HashMap::new(),
        })
    }

    pub fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    /// Runs `module.verify()`. Call after [`CodeGen::generate`].
    pub fn verify(&self) -> Result<(), CodegenError> {
        self.module
            .verify()
            .map_err(|err| CodegenError::ModuleVerificationFailed {
                module: self.module.get_name().to_string_lossy().into_owned(),
                detail: err.to_string(),
            })
    }

    /// Prints the module as LLVM IR text (useful for tests and `--emit IR`).
    pub fn print_ir(&self) -> String {
        self.module.print_to_string().to_string_lossy().into_owned()
    }

    /// Generates IR for the whole MIR program
    pub fn generate(&mut self) -> Result<(), CodegenError> {
        self.register_struct_layouts();
        self.declare_externs();
        self.declare_functions();
        self.emit_function_bodies();
        self.emit_main_wrapper();

        if self.options.mode == CompilationMode::Release {
            self.run_optimization_passes()?;
        }

        Ok(())
    }

    /// Emits the module to an object file (`.o`).
    pub fn emit_object(&self, path: &Path) -> Result<(), CodegenError> {
        self.machine
            .write_to_file(&self.module, FileType::Object, path)
            .map_err(|err| CodegenError::EmitFailed {
                kind: "object",
                path: path.display().to_string(),
                detail: err.to_string(),
            })
    }

    /// Emits the module to an assembly file (`.s`).
    pub fn emit_assembly(&self, path: &Path) -> Result<(), CodegenError> {
        self.machine
            .write_to_file(&self.module, FileType::Assembly, path)
            .map_err(|err| CodegenError::EmitFailed {
                kind: "assembly",
                path: path.display().to_string(),
                detail: err.to_string(),
            })
    }

    /// Emits the module as LLVM IR text (`.ll`).
    pub fn emit_ir(&self, path: &Path) -> Result<(), CodegenError> {
        self.module
            .print_to_file(path)
            .map_err(|err| CodegenError::EmitFailed {
                kind: "IR",
                path: path.display().to_string(),
                detail: err.to_string(),
            })
    }

    fn run_optimization_passes(&self) -> Result<(), CodegenError> {
        let options = inkwell::passes::PassBuilderOptions::create();
        options.set_verify_each(false);
        options.set_loop_vectorization(true);
        options.set_loop_slp_vectorization(true);
        options.set_loop_unrolling(true);
        options.set_merge_functions(true);

        self.module
            .run_passes("default<O3>", &self.machine, options)
            .map_err(|err| CodegenError::PassPipelineFailed {
                detail: err.to_string(),
            })
    }

