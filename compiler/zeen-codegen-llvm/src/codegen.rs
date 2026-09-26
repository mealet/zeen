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

    fn register_struct_layouts(&mut self) {
        for &ty in self.program.struct_layouts.keys() {
            let name = self.mangle_struct_name(ty);
            let opaque = self.context.opaque_struct_type(&name);
            self.struct_types.insert(ty, opaque);
        }

        let mut bodies: Vec<(TypeId, Vec<BasicTypeEnum<'ctx>>)> = Vec::new();
        for &ty in self.program.struct_layouts.keys() {
            let layout = &self.program.struct_layouts[&ty];
            let fields: Vec<BasicTypeEnum<'ctx>> = layout
                .fields
                .iter()
                .map(|f| self.map_basic_type(f.ty))
                .collect();
            bodies.push((ty, fields));
        }

        for (ty, fields) in bodies {
            self.struct_types[&ty].set_body(&fields, false);
        }
    }

    fn declare_externs(&mut self) {
        for decl in &self.program.extern_fns {
            let param_types: Vec<BasicMetadataTypeEnum<'ctx>> = decl
                .param_types
                .iter()
                .map(|&t| self.map_basic_type(t).into())
                .collect();
            let ret = self.map_ret_type(decl.ret_ty);
            let fn_type = self.make_fn_type(ret, &param_types, decl.is_variadic);
            if self.module.get_function(&decl.symbol_name).is_none() {
                self.module.add_function(
                    &decl.symbol_name,
                    fn_type,
                    Some(inkwell::module::Linkage::External),
                );
            }
        }

        for decl in &self.program.extern_vars {
            let global =
                self.module
                    .add_global(self.map_basic_type(decl.ty), None, &decl.symbol_name);
            global.set_linkage(inkwell::module::Linkage::External);
        }
    }

    fn declare_functions(&mut self) {
        let mut ids: Vec<MirFunctionId> = self.program.functions.keys().copied().collect();
        ids.sort_by_key(|id| id.0);

        for id in ids {
            let func = &self.program.functions[&id];
            let name = self.function_symbol_name(id, func);

            let param_types: Vec<BasicMetadataTypeEnum<'ctx>> = func
                .params
                .iter()
                .map(|&local| self.map_basic_type(func.local(local).ty).into())
                .collect();
            let ret = self.map_ret_type(func.ret_ty);
            let fn_type = self.make_fn_type(ret, &param_types, false);

            let function = self.module.add_function(&name, fn_type, None);
            self.functions.insert(id, function);
        }
    }

    fn emit_function_bodies(&mut self) {
        let mut ids: Vec<MirFunctionId> = self.program.functions.keys().copied().collect();
        ids.sort_by_key(|id| id.0);

        for id in ids {
            self.emit_function(id);
        }
    }

    fn emit_function(&mut self, id: MirFunctionId) {
        let func = &self.program.functions[&id];
        let function = self.functions[&id];

        let entry = self.context.append_basic_block(function, "entry");
        self.locals.clear();
        self.blocks.clear();
        self.builder.position_at_end(entry);

        for (idx, decl) in func.locals.iter().enumerate() {
            if self.is_void_ty(decl.ty) {
                continue;
            }
            let alloca = self
                .builder
                .build_alloca(self.map_basic_type(decl.ty), &format!("%{idx}"))
                .unwrap();
            self.locals.insert(LocalId(idx as u32), alloca);
        }

        for (i, &param_local) in func.params.iter().enumerate() {
            let Some(arg) = function.get_nth_param(i as u32) else {
                continue;
            };
            let ptr = self.locals[&param_local];
            self.builder.build_store(ptr, arg).unwrap();
        }

        for idx in 1..func.blocks.len() {
            let block = self
                .context
                .append_basic_block(function, &format!("bb{idx}"));
            self.blocks.insert(BlockId(idx as u32), block);
        }
        self.blocks.insert(func.entry_block, entry);

        for (idx, block) in func.blocks.iter().enumerate() {
            let block_id = BlockId(idx as u32);
            self.builder.position_at_end(self.blocks[&block_id]);
            for stmt in &block.statements {
                self.emit_statement(stmt, func);
            }
            self.emit_terminator(&block.terminator, func, id);
        }

        assert!(
            function.verify(false),
            "LLVM failed to verify function:\n{}",
            self.print_ir()
        );
    }

    fn emit_main_wrapper(&mut self) {
        let Some(main_fn) = self.options.main_fn else {
            return;
        };
        let zeen_main = self.functions[&main_fn];
        let ret_ty = self.program.functions[&main_fn].ret_ty;

        let main_ty = self.context.i32_type().fn_type(&[], false);
        let main = self.module.add_function("main", main_ty, None);
        let entry = self.context.append_basic_block(main, "entry");
        self.builder.position_at_end(entry);

        if self.is_integer_return(ret_ty) {
            let call = self.builder.build_call(zeen_main, &[], "").unwrap();
            let value = call.try_as_basic_value().unwrap_basic();
            let value = self.coerce_to_i32(value, ret_ty);
            self.builder.build_return(Some(&value)).unwrap();
        } else {
            self.builder.build_call(zeen_main, &[], "").unwrap();
            self.builder
                .build_return(Some(&self.context.i32_type().const_int(0, false)))
                .unwrap();
        }

        assert!(main.verify(false), "LLVM failed to verify main wrapper");
    }

    fn map_type(&self, ty: TypeId) -> AnyTypeEnum<'ctx> {
        match self.typecheck.interner.get(ty).clone() {
            Type::Builtin(BuiltinType::void) => self.context.void_type().into(),
            Type::Builtin(b) => self.any_from_basic(self.map_builtin(b)),

            Type::IntLiteral => self.context.i32_type().into(),
            Type::FloatLiteral => self.context.f64_type().into(),

            Type::Struct { .. } | Type::Slice { .. } => self.struct_types[&ty].into(),

            Type::Enum { .. } => self.context.i32_type().into(),
            // Never reach codegen on a valid program.
            Type::Interface { .. } | Type::InterfaceSelfPlaceholder(_) | Type::GenericParam(_) => {
                self.context.i32_type().into()
            }

            Type::Pointer { .. } | Type::ManyPointer { .. } => {
                self.context.ptr_type(AddressSpace::default()).into()
            }

            Type::Array { element, len } => self
                .map_basic_type(element)
                .array_type(len.unwrap_or(0) as u32)
                .into(),

            Type::Fn { .. } => self.context.ptr_type(AddressSpace::default()).into(),

            Type::Void | Type::Never => self.context.void_type().into(),
            Type::Error => self.context.i32_type().into(),
        }
    }

    fn map_builtin(&self, b: BuiltinType) -> BasicTypeEnum<'ctx> {
        use BuiltinType::*;
        match b {
            i8 | u8 | char => self.context.i8_type().into(),
            i16 | u16 => self.context.i16_type().into(),
            i32 | u32 => self.context.i32_type().into(),
            i64 | u64 => self.context.i64_type().into(),
            isize | usize => self
                .context
                .ptr_sized_int_type(&self.target_data, None)
                .into(),
            f32 => self.context.f32_type().into(),
            f64 => self.context.f64_type().into(),
            bool => self.context.bool_type().into(),
            void => panic!("void is not a basic value type"),
        }
    }

    fn map_ret_type(&self, ty: TypeId) -> AnyTypeEnum<'ctx> {
        match self.typecheck.interner.get(ty) {
            Type::Void | Type::Never => self.context.void_type().into(),
            _ => self.map_type(ty),
        }
    }

    fn map_basic_type(&self, ty: TypeId) -> BasicTypeEnum<'ctx> {
        match self.map_type(ty) {
            AnyTypeEnum::ArrayType(t) => t.into(),
            AnyTypeEnum::FloatType(t) => t.into(),
            AnyTypeEnum::FunctionType(_) => panic!("function type used as a value type"),
            AnyTypeEnum::IntType(t) => t.into(),
            AnyTypeEnum::PointerType(t) => t.into(),
            AnyTypeEnum::StructType(t) => t.into(),
            AnyTypeEnum::VectorType(t) => t.into(),
            AnyTypeEnum::ScalableVectorType(t) => t.into(),
            AnyTypeEnum::VoidType(_) => panic!("void used as a value type"),
        }
    }

    fn any_from_basic(&self, basic: BasicTypeEnum<'ctx>) -> AnyTypeEnum<'ctx> {
        match basic {
            BasicTypeEnum::ArrayType(t) => t.into(),
            BasicTypeEnum::FloatType(t) => t.into(),
            BasicTypeEnum::IntType(t) => t.into(),
            BasicTypeEnum::PointerType(t) => t.into(),
            BasicTypeEnum::StructType(t) => t.into(),
            BasicTypeEnum::VectorType(t) => t.into(),
            BasicTypeEnum::ScalableVectorType(t) => t.into(),
        }
    }

    fn make_fn_type(
        &self,
        ret: AnyTypeEnum<'ctx>,
        params: &[BasicMetadataTypeEnum<'ctx>],
        is_var_args: bool,
    ) -> FunctionType<'ctx> {
        use AnyTypeEnum::*;
        match ret {
            ArrayType(t) => t.fn_type(params, is_var_args),
            FloatType(t) => t.fn_type(params, is_var_args),
            FunctionType(_) => panic!("function type used as a return type"),
            IntType(t) => t.fn_type(params, is_var_args),
            PointerType(t) => t.fn_type(params, is_var_args),
            StructType(t) => t.fn_type(params, is_var_args),
            VectorType(t) => t.fn_type(params, is_var_args),
            ScalableVectorType(t) => t.fn_type(params, is_var_args),
            VoidType(t) => t.fn_type(params, is_var_args),
        }
    }

    fn is_signed(&self, ty: TypeId) -> bool {
        matches!(
            self.typecheck.interner.get(ty),
            Type::Builtin(b)
                if matches!(b, BuiltinType::i8 | BuiltinType::i16 | BuiltinType::i32 | BuiltinType::i64 | BuiltinType::isize)
        )
    }

    fn is_void_ty(&self, ty: TypeId) -> bool {
        matches!(self.typecheck.interner.get(ty), Type::Void | Type::Never)
    }

    fn is_signed_type(&self, ty: &Type) -> bool {
        matches!(
            ty,
            Type::Builtin(b)
                if matches!(b, BuiltinType::i8 | BuiltinType::i16 | BuiltinType::i32 | BuiltinType::i64 | BuiltinType::isize)
        )
    }

    fn is_integer_return(&self, ty: TypeId) -> bool {
        matches!(self.typecheck.interner.get(ty).clone(), Type::Builtin(b) if builtin_is_integer(b))
    }

    fn coerce_to_i32(&self, value: BasicValueEnum<'ctx>, ty: TypeId) -> BasicValueEnum<'ctx> {
        let int = value.into_int_value();
        let i32_ty = self.context.i32_type();
        let src_w = int.get_type().get_bit_width();
        let dst_w = i32_ty.get_bit_width();

        if src_w > dst_w {
            self.builder
                .build_int_truncate(int, i32_ty, "")
                .unwrap()
                .into()
        } else if src_w < dst_w {
            if self.is_signed(ty) {
                self.builder
                    .build_int_s_extend(int, i32_ty, "")
                    .unwrap()
                    .into()
            } else {
                self.builder
                    .build_int_z_extend(int, i32_ty, "")
                    .unwrap()
                    .into()
            }
        } else {
            int.into()
        }
    }

    fn emit_statement(&mut self, stmt: &MirStatement, func: &MirFunction) {
        match stmt {
            MirStatement::Assign { place, rvalue, .. } => {
                let place_ty = self.place_type(place, func);
                let value = self.rvalue_value(rvalue, place_ty, func);
                self.store_place(place, value, func);
            }

            MirStatement::Discard(operand) => {
                let is_void = match self.operand_type(operand, func) {
                    Some(ty) => {
                        matches!(self.typecheck.interner.get(ty), Type::Void | Type::Never)
                    }
                    None => false,
                };
                if !is_void {
                    let _ = self.operand_value(operand, None, func);
                }
            }

            MirStatement::Drop(_) => {
                unimplemented!("RAII/drop is not implemented in codegen yet")
            }

            MirStatement::StorageLive(_) | MirStatement::StorageDead(_) | MirStatement::Nop => {}
        }
    }

    fn rvalue_value(
        &mut self,
        rvalue: &Rvalue,
        expected_ty: TypeId,
        func: &MirFunction,
    ) -> BasicValueEnum<'ctx> {
        match rvalue {
            Rvalue::Use(operand) => self.operand_value(operand, Some(expected_ty), func),

            Rvalue::BinaryOp { op, lhs, rhs } => self.binary_op(*op, lhs, rhs, expected_ty, func),

            Rvalue::UnaryOp { op, operand } => self.unary_op(*op, operand, expected_ty, func),

            Rvalue::Ref { place, .. } => self.place_ptr(place, func).into(),

            Rvalue::Cast { operand, target } => self.cast_op(operand, *target, func),

            Rvalue::SizeOf(ty) => self.size_of(*ty),

            Rvalue::AlignOf(ty) => self.align_of(*ty),

            Rvalue::Aggregate { .. } => {
                unimplemented!("struct/array/slice aggregates are not implemented in codegen yet")
            }

            Rvalue::Discriminant(_) => {
                unimplemented!("enum discriminants are not implemented in codegen yet")
            }
        }
    }

    fn operand_value(
        &mut self,
        operand: &Operand,
        expected: Option<TypeId>,
        func: &MirFunction,
    ) -> BasicValueEnum<'ctx> {
        match operand {
            Operand::Copy(place, _) | Operand::Move(place, _) => self.load_place(place, func),
            Operand::Constant(value, _) => self.const_value(value, expected, func),
        }
    }

    fn operand_type(&self, operand: &Operand, func: &MirFunction) -> Option<TypeId> {
        match operand {
            Operand::Copy(place, _) | Operand::Move(place, _) => Some(self.place_type(place, func)),
            Operand::Constant(_, _) => None,
        }
    }

    fn const_value(
        &mut self,
        c: &ConstValue,
        expected: Option<TypeId>,
        _func: &MirFunction,
    ) -> BasicValueEnum<'ctx> {
        match c {
            ConstValue::Int(n) => {
                let int_ty = match expected {
                    Some(ty) => self.map_basic_type(ty).into_int_type(),
                    None => self.context.i32_type(),
                };
                let signed = expected.map(|ty| self.is_signed(ty)).unwrap_or(true);
                int_ty.const_int(*n as u64, signed).into()
            }

            ConstValue::Float(f) => self.context.f64_type().const_float(*f).into(),

            ConstValue::Bool(b) => self.context.bool_type().const_int(*b as u64, false).into(),

            ConstValue::Char(ch) => self.context.i8_type().const_int(*ch as u64, false).into(),

            ConstValue::Str(spur) => {
                let content = self.resolve_spur(*spur);
                self.get_str_global(&content).as_pointer_value().into()
            }

            ConstValue::NullPtr => self
                .context
                .ptr_type(AddressSpace::default())
                .const_null()
                .into(),

            ConstValue::Void => unimplemented!("void constants are not value-typed"),
        }
    }

    fn binary_op(
        &mut self,
        op: BinaryOp,
        lhs: &Operand,
        rhs: &Operand,
        ty: TypeId,
        func: &MirFunction,
    ) -> BasicValueEnum<'ctx> {
        let is_float = matches!(
            self.typecheck.interner.get(ty),
            Type::Builtin(BuiltinType::f32 | BuiltinType::f64) | Type::FloatLiteral
        );

        let lhs_v = self.operand_value(lhs, Some(ty), func);
        let rhs_v = self.operand_value(rhs, Some(ty), func);

        if is_float {
            let l = lhs_v.into_float_value();
            let r = rhs_v.into_float_value();
            let b = &self.builder;
            return match op {
                BinaryOp::Add => b.build_float_add(l, r, "").unwrap().into(),
                BinaryOp::Sub => b.build_float_sub(l, r, "").unwrap().into(),
                BinaryOp::Mul => b.build_float_mul(l, r, "").unwrap().into(),
                BinaryOp::Div => b.build_float_div(l, r, "").unwrap().into(),
                BinaryOp::Mod => b.build_float_rem(l, r, "").unwrap().into(),
                BinaryOp::Eq => b
                    .build_float_compare(FloatPredicate::OEQ, l, r, "")
                    .unwrap()
                    .into(),
                BinaryOp::Ne => b
                    .build_float_compare(FloatPredicate::ONE, l, r, "")
                    .unwrap()
                    .into(),
                BinaryOp::Lt => b
                    .build_float_compare(FloatPredicate::OLT, l, r, "")
                    .unwrap()
                    .into(),
                BinaryOp::Gt => b
                    .build_float_compare(FloatPredicate::OGT, l, r, "")
                    .unwrap()
                    .into(),
                BinaryOp::Le => b
                    .build_float_compare(FloatPredicate::OLE, l, r, "")
                    .unwrap()
                    .into(),
                BinaryOp::Ge => b
                    .build_float_compare(FloatPredicate::OGE, l, r, "")
                    .unwrap()
                    .into(),
                _ => unimplemented!("float binary op {op:?}"),
            };
        }

        let l = lhs_v.into_int_value();
        let r = rhs_v.into_int_value();
        let signed = self.is_signed(ty);
        let b = &self.builder;

        match op {
            BinaryOp::Add => b.build_int_add(l, r, "").unwrap().into(),
            BinaryOp::Sub => b.build_int_sub(l, r, "").unwrap().into(),
            BinaryOp::Mul => b.build_int_mul(l, r, "").unwrap().into(),
            BinaryOp::Div => if signed {
                b.build_int_signed_div(l, r, "").unwrap()
            } else {
                b.build_int_unsigned_div(l, r, "").unwrap()
            }
            .into(),
            BinaryOp::Mod => if signed {
                b.build_int_signed_rem(l, r, "").unwrap()
            } else {
                b.build_int_unsigned_rem(l, r, "").unwrap()
            }
            .into(),
            BinaryOp::BitAnd => b.build_and(l, r, "").unwrap().into(),
            BinaryOp::BitOr => b.build_or(l, r, "").unwrap().into(),
            BinaryOp::BitXor => b.build_xor(l, r, "").unwrap().into(),
            BinaryOp::Shl => b.build_left_shift(l, r, "").unwrap().into(),
            BinaryOp::Shr => b.build_right_shift(l, r, signed, "").unwrap().into(),

            BinaryOp::Eq => b
                .build_int_compare(IntPredicate::EQ, l, r, "")
                .unwrap()
                .into(),
            BinaryOp::Ne => b
                .build_int_compare(IntPredicate::NE, l, r, "")
                .unwrap()
                .into(),
            BinaryOp::Lt => b
                .build_int_compare(
                    if signed {
                        IntPredicate::SLT
                    } else {
                        IntPredicate::ULT
                    },
                    l,
                    r,
                    "",
                )
                .unwrap()
                .into(),
            BinaryOp::Gt => b
                .build_int_compare(
                    if signed {
                        IntPredicate::SGT
                    } else {
                        IntPredicate::UGT
                    },
                    l,
                    r,
                    "",
                )
                .unwrap()
                .into(),
            BinaryOp::Le => b
                .build_int_compare(
                    if signed {
                        IntPredicate::SLE
                    } else {
                        IntPredicate::ULE
                    },
                    l,
                    r,
                    "",
                )
                .unwrap()
                .into(),
            BinaryOp::Ge => b
                .build_int_compare(
                    if signed {
                        IntPredicate::SGE
                    } else {
                        IntPredicate::UGE
                    },
                    l,
                    r,
                    "",
                )
                .unwrap()
                .into(),

            BinaryOp::LogicalAnd => b.build_and(l, r, "").unwrap().into(),
            BinaryOp::LogicalOr => b.build_or(l, r, "").unwrap().into(),
        }
    }

    fn unary_op(
        &mut self,
        op: UnaryOp,
        operand: &Operand,
        expected_ty: TypeId,
        func: &MirFunction,
    ) -> BasicValueEnum<'ctx> {
        match op {
            UnaryOp::Neg => {
                let v = self.operand_value(operand, Some(expected_ty), func);
                if matches!(
                    self.typecheck.interner.get(expected_ty),
                    Type::Builtin(BuiltinType::f32 | BuiltinType::f64)
                ) {
                    self.builder
                        .build_float_neg(v.into_float_value(), "")
                        .unwrap()
                        .into()
                } else {
                    self.builder
                        .build_int_neg(v.into_int_value(), "")
                        .unwrap()
                        .into()
                }
            }
            UnaryOp::Not => {
                let v = self.operand_value(operand, Some(expected_ty), func);
                self.builder
                    .build_not(v.into_int_value(), "")
                    .unwrap()
                    .into()
            }
            UnaryOp::BitNot => {
                let v = self
                    .operand_value(operand, Some(expected_ty), func)
                    .into_int_value();
                let ones = v.get_type().const_int(u64::MAX, false);
                self.builder.build_xor(v, ones, "").unwrap().into()
            }
            UnaryOp::Deref | UnaryOp::AddrOf => {
                unimplemented!("unary deref/addrof are not implemented in codegen yet")
            }
        }
    }

    fn size_of(&self, ty: TypeId) -> BasicValueEnum<'ctx> {
        let size = self.map_basic_type(ty).size_of().expect("sized type");
        self.bitcast_to_usize(size)
    }

    fn align_of(&self, ty: TypeId) -> BasicValueEnum<'ctx> {
        let align = self.target_data.get_abi_alignment(&self.map_type(ty));
        self.context
            .ptr_sized_int_type(&self.target_data, None)
            .const_int(align as u64, false)
            .into()
    }

    fn bitcast_to_usize(&self, value: IntValue<'ctx>) -> BasicValueEnum<'ctx> {
        let usize_ty = self.context.ptr_sized_int_type(&self.target_data, None);
        let src_w = value.get_type().get_bit_width();
        let dst_w = usize_ty.get_bit_width();
        if src_w == dst_w {
            value.into()
        } else if src_w < dst_w {
            self.builder
                .build_int_z_extend(value, usize_ty, "")
                .unwrap()
                .into()
        } else {
            self.builder
                .build_int_truncate(value, usize_ty, "")
                .unwrap()
                .into()
        }
    }

    fn cast_op(
        &mut self,
        operand: &Operand,
        target: TypeId,
        func: &MirFunction,
    ) -> BasicValueEnum<'ctx> {
        match operand {
            Operand::Constant(c, _) => {
                let src_ty = match c {
                    ConstValue::Int(_) => Type::Builtin(BuiltinType::i32),
                    ConstValue::Float(_) => Type::Builtin(BuiltinType::f64),
                    ConstValue::Bool(_) => Type::Builtin(BuiltinType::bool),
                    ConstValue::Char(_) => Type::Builtin(BuiltinType::char),
                    ConstValue::NullPtr => Type::Pointer {
                        inner: TypeId(0),
                        is_const: false,
                    },
                    _ => unimplemented!("cast of an unsupported constant"),
                };
                let value = self.const_value(c, None, func);
                self.cast_value(value, &src_ty, target)
            }
            _ => {
                let src_ty = self
                    .operand_type(operand, func)
                    .expect("typed cast operand");
                let value = self.operand_value(operand, Some(src_ty), func);
                let src = self.typecheck.interner.get(src_ty).clone();
                self.cast_value(value, &src, target)
            }
        }
    }

    fn cast_value(
        &self,
        value: BasicValueEnum<'ctx>,
        src: &Type,
        dst: TypeId,
    ) -> BasicValueEnum<'ctx> {
        use Type::*;

        let src_ty = src.clone();
        let dst_ty = self.typecheck.interner.get(dst).clone();

        match (src_ty, dst_ty.clone()) {
            (Builtin(a), Builtin(b)) if builtin_is_integer(a) && builtin_is_integer(b) => {
                let int = value.into_int_value();
                let dst_int = self.map_basic_type(dst).into_int_type();
                let src_w = int.get_type().get_bit_width();
                let dst_w = dst_int.get_bit_width();
                if src_w == dst_w {
                    self.builder.build_bit_cast(int, dst_int, "").unwrap()
                } else if src_w < dst_w {
                    if self.is_signed_type(src) {
                        self.builder
                            .build_int_s_extend(int, dst_int, "")
                            .unwrap()
                            .into()
                    } else {
                        self.builder
                            .build_int_z_extend(int, dst_int, "")
                            .unwrap()
                            .into()
                    }
                } else {
                    self.builder
                        .build_int_truncate(int, dst_int, "")
                        .unwrap()
                        .into()
                }
            }

            (Builtin(a), Builtin(b)) if builtin_is_float(a) && builtin_is_float(b) => {
                let float = value.into_float_value();
                let dst_float = self.map_basic_type(dst).into_float_type();
                if float.get_type().get_bit_width() > dst_float.get_bit_width() {
                    self.builder
                        .build_float_trunc(float, dst_float, "")
                        .unwrap()
                        .into()
                } else {
                    self.builder
                        .build_float_ext(float, dst_float, "")
                        .unwrap()
                        .into()
                }
            }

            (Builtin(a), Builtin(b)) if builtin_is_float(a) && builtin_is_integer(b) => {
                let float = value.into_float_value();
                let dst_int = self.map_basic_type(dst).into_int_type();
                if self.is_signed_type(&dst_ty) {
                    self.builder
                        .build_float_to_signed_int(float, dst_int, "")
                        .unwrap()
                        .into()
                } else {
                    self.builder
                        .build_float_to_unsigned_int(float, dst_int, "")
                        .unwrap()
                        .into()
                }
            }

            (Builtin(a), Builtin(b)) if builtin_is_integer(a) && builtin_is_float(b) => {
                let int = value.into_int_value();
                let dst_float = self.map_basic_type(dst).into_float_type();
                if self.is_signed_type(src) {
                    self.builder
                        .build_signed_int_to_float(int, dst_float, "")
                        .unwrap()
                        .into()
                } else {
                    self.builder
                        .build_unsigned_int_to_float(int, dst_float, "")
                        .unwrap()
                        .into()
                }
            }

            (Pointer { .. } | ManyPointer { .. }, Builtin(b)) if builtin_is_integer(b) => {
                let dst_int = self.map_basic_type(dst).into_int_type();
                self.builder
                    .build_ptr_to_int(value.into_pointer_value(), dst_int, "")
                    .unwrap()
                    .into()
            }

            (Builtin(b), Pointer { .. } | ManyPointer { .. }) if builtin_is_integer(b) => {
                let dst_ptr = self.context.ptr_type(AddressSpace::default());
                self.builder
                    .build_int_to_ptr(value.into_int_value(), dst_ptr, "")
                    .unwrap()
                    .into()
            }

            (Pointer { .. } | ManyPointer { .. }, Pointer { .. } | ManyPointer { .. }) => {
                let dst_ptr = self.context.ptr_type(AddressSpace::default());
                self.builder
                    .build_pointer_cast(value.into_pointer_value(), dst_ptr, "")
                    .unwrap()
                    .into()
            }

            (Builtin(b), Builtin(bb)) if builtin_is_integer(b) && bb == BuiltinType::bool => {
                let int = value.into_int_value();
                let zero = int.get_type().const_zero();
                self.builder
                    .build_int_compare(IntPredicate::NE, int, zero, "")
                    .unwrap()
                    .into()
            }

            _ => {
                unimplemented!("cast from {src:?} to {dst_ty:?} is not implemented in codegen yet")
            }
        }
    }

    fn emit_terminator(&mut self, term: &Terminator, func: &MirFunction, fn_id: MirFunctionId) {
        let _ = fn_id;
        match term {
            Terminator::Goto(block) => {
                let target = self.blocks[block];
                self.builder.build_unconditional_branch(target).unwrap();
            }

            Terminator::SwitchInt {
                discriminant,
                targets,
                otherwise,
            } => {
                let value = self
                    .operand_value(discriminant, None, func)
                    .into_int_value();
                let otherwise = self.blocks[otherwise];
                let cases: Vec<(IntValue<'ctx>, BasicBlock<'ctx>)> = targets
                    .iter()
                    .map(|&(v, block)| {
                        (
                            value.get_type().const_int(v as u64, false),
                            self.blocks[&block],
                        )
                    })
                    .collect();
                self.builder.build_switch(value, otherwise, &cases).unwrap();
            }

            Terminator::Call {
                func: target,
                args,
                destination,
                target: next,
                ..
            } => {
                self.emit_call(target, args, destination, *next, func);
            }

            Terminator::MacroCall {
                kind,
                format_chunks,
                args,
                destination,
                target,
                ..
            } => {
                self.emit_macro_call(
                    *kind,
                    format_chunks.as_deref(),
                    args,
                    destination,
                    *target,
                    func,
                );
            }

            Terminator::Return(operand) => {
                if matches!(operand, Operand::Constant(ConstValue::Void, _)) {
                    self.builder.build_return(None).unwrap();
                } else {
                    let value = self.operand_value(operand, Some(func.ret_ty), func);
                    self.builder.build_return(Some(&value)).unwrap();
                }
            }

            Terminator::Unreachable => {
                self.builder.build_unreachable().unwrap();
            }
        }
    }

    fn emit_call(
        &mut self,
        target: &CallTarget,
        args: &[Operand],
        destination: &Place,
        next: Option<BlockId>,
        func: &MirFunction,
    ) {
        let arg_values: Vec<BasicMetadataValueEnum<'ctx>> = args
            .iter()
            .map(|arg| {
                let ty = self.operand_type(arg, func);
                self.operand_value(arg, ty, func).into()
            })
            .collect();

        let call = match target {
            CallTarget::Direct(id) => {
                let callee = self.functions[id];
                self.builder.build_call(callee, &arg_values, "").unwrap()
            }
            CallTarget::Extern(idx) => {
                let decl = &self.program.extern_fns[*idx];
                let callee = self.module.get_function(&decl.symbol_name).unwrap();
                self.builder.build_call(callee, &arg_values, "").unwrap()
            }
            CallTarget::Indirect(operand) => {
                let fptr = self.operand_value(operand, None, func).into_pointer_value();
                let op_ty = self
                    .operand_type(operand, func)
                    .expect("fn pointer operand");
                let fn_ty = self.map_fn_pointer_type(op_ty);
                self.builder
                    .build_indirect_call(fn_ty, fptr, &arg_values, "")
                    .unwrap()
            }
        };

        match call.try_as_basic_value() {
            ValueKind::Basic(value) => self.store_place(destination, value, func),
            ValueKind::Instruction(_) => {}
        }

        if let Some(next) = next {
            let block = self.blocks[&next];
            self.builder.build_unconditional_branch(block).unwrap();
        }
    }

    fn map_fn_pointer_type(&self, ty: TypeId) -> FunctionType<'ctx> {
        match self.typecheck.interner.get(ty).clone() {
            Type::Fn { params, ret } => {
                let params: Vec<BasicMetadataTypeEnum<'ctx>> = params
                    .iter()
                    .map(|&p| self.map_basic_type(p).into())
                    .collect();
                self.make_fn_type(self.map_ret_type(ret), &params, false)
            }
            _ => unimplemented!("indirect call through a non-fn value"),
        }
    }

    fn emit_macro_call(
        &mut self,
        kind: HirMacroKind,
        chunks: Option<&[FormatChunk]>,
        args: &[Operand],
        destination: &Place,
        target: Option<BlockId>,
        func: &MirFunction,
    ) {
        let _ = destination;
        match kind {
            HirMacroKind::Print | HirMacroKind::Println => {
                let (format, values) = self.build_format(chunks.unwrap_or(&[]), args, func);
                let format = if kind == HirMacroKind::Println {
                    format!("{format}\n")
                } else {
                    format
                };

                let printf = self.get_or_declare_runtime_fn(
                    "printf",
                    self.context.i32_type().into(),
                    &[self.context.ptr_type(AddressSpace::default()).into()],
                    true,
                );

                let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> =
                    vec![self.get_str_global(&format).as_pointer_value().into()];
                call_args.extend(values.into_iter().map(BasicMetadataValueEnum::from));

                self.builder.build_call(printf, &call_args, "").unwrap();

                if let Some(next) = target {
                    let block = self.blocks[&next];
                    self.builder.build_unconditional_branch(block).unwrap();
                }
            }

            HirMacroKind::Panic => {
                let default_chunks = [FormatChunk::Literal("panic".to_string())];
                let chunks = chunks.unwrap_or(&default_chunks);
                let (format, values) = self.build_format(chunks, args, func);

                let panic_fn = self.get_or_declare_runtime_fn(
                    "zeen.panic_message",
                    self.context.void_type().into(),
                    &[self.context.ptr_type(AddressSpace::default()).into()],
                    true,
                );

                let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> =
                    vec![self.get_str_global(&format).as_pointer_value().into()];
                call_args.extend(values.into_iter().map(BasicMetadataValueEnum::from));

                self.builder.build_call(panic_fn, &call_args, "").unwrap();
                self.builder.build_unreachable().unwrap();
            }

            _ => unimplemented!("macro {kind:?} is not implemented in codegen yet"),
        }
    }

    /// Builds a printf-style format string and the converted argument values
    /// from `format_chunks` + operands.
    fn build_format(
        &mut self,
        chunks: &[FormatChunk],
        args: &[Operand],
        func: &MirFunction,
    ) -> (String, Vec<BasicValueEnum<'ctx>>) {
        let mut format = String::new();
        let mut values: Vec<BasicValueEnum<'ctx>> = Vec::new();
        let mut arg_iter = args.iter();

        for chunk in chunks {
            match chunk {
                FormatChunk::Literal(text) => format.push_str(&text.replace('%', "%%")),
                FormatChunk::Arg(spec) => {
                    let Some(operand) = arg_iter.next() else {
                        break;
                    };

                    let (specifier, value) = match (operand, spec) {
                        (Operand::Constant(c, _), _) => {
                            let value = self.const_value(c, None, func);
                            let specifier = match (c, spec) {
                                (ConstValue::Float(_), FormatSpec::Float { precision }) => {
                                    format!("%.{precision}f")
                                }
                                (ConstValue::Float(_), _) => "%f".to_string(),
                                (ConstValue::Str(_), _) => "%s".to_string(),
                                (ConstValue::Char(_), _) => "%c".to_string(),
                                (ConstValue::Bool(_), _) => "%d".to_string(),
                                (ConstValue::Int(_), FormatSpec::Hex) => "%x".to_string(),
                                (ConstValue::Int(_), FormatSpec::Oct) => "%o".to_string(),
                                (ConstValue::Int(_), FormatSpec::Bin) => "%x".to_string(),
                                _ => "%d".to_string(),
                            };
                            (specifier, value)
                        }
                        _ => {
                            let ty = self.operand_type(operand, func).expect("typed format arg");
                            let value = self.operand_value(operand, Some(ty), func);
                            let specifier = match spec {
                                FormatSpec::Display | FormatSpec::Debug => {
                                    self.display_specifier(ty)
                                }
                                FormatSpec::Hex => "%x".to_string(),
                                FormatSpec::Oct => "%o".to_string(),
                                FormatSpec::Bin => "%x".to_string(),
                                FormatSpec::Float { precision } => format!("%.{precision}f"),
                            };
                            (specifier, value)
                        }
                    };

                    format.push_str(&specifier);
                    values.push(value);
                }
            }
        }

        (format, values)
    }

    fn display_specifier(&self, ty: TypeId) -> String {
        match self.typecheck.interner.get(ty).clone() {
            Type::Builtin(BuiltinType::f32 | BuiltinType::f64) | Type::FloatLiteral => "%f".into(),
            Type::Builtin(b) if builtin_is_integer(b) => "%d".into(),
            Type::Pointer { .. } | Type::ManyPointer { .. } | Type::Slice { .. } => "%s".into(),
            _ => "%d".into(),
        }
    }

    fn store_place(&mut self, place: &Place, value: BasicValueEnum<'ctx>, func: &MirFunction) {
        let ptr = self.place_ptr(place, func);
        self.builder.build_store(ptr, value).unwrap();
    }

    fn load_place(&self, place: &Place, func: &MirFunction) -> BasicValueEnum<'ctx> {
        let ptr = self.place_ptr(place, func);
        let ty = self.place_type(place, func);
        self.builder
            .build_load(self.map_basic_type(ty), ptr, "")
            .unwrap()
    }

    fn place_ptr(&self, place: &Place, func: &MirFunction) -> PointerValue<'ctx> {
        let mut ptr = self.locals[&place.local];
        let mut cur_ty = func.local(place.local).ty;

        for elem in &place.projection {
            match elem {
                PlaceElem::Field(field_def) => {
                    let struct_ty = self.map_basic_type(cur_ty);
                    let (index, field_ty) = self.field_index_and_type(cur_ty, *field_def);
                    ptr = self
                        .builder
                        .build_struct_gep(struct_ty, ptr, index, "")
                        .unwrap();
                    cur_ty = field_ty;
                }

                PlaceElem::Index(index_local) => {
                    let usize_ty = self.context.ptr_sized_int_type(&self.target_data, None);
                    let index = self
                        .builder
                        .build_load(usize_ty, self.locals[index_local], "")
                        .unwrap()
                        .into_int_value();
                    let elem_ty = self.map_basic_type(self.index_element_type(cur_ty));
                    ptr = unsafe {
                        self.builder
                            .build_in_bounds_gep(elem_ty, ptr, &[index], "")
                            .unwrap()
                    };
                    cur_ty = self.index_element_type(cur_ty);
                }

                PlaceElem::Deref => {
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    ptr = self
                        .builder
                        .build_load(ptr_ty, ptr, "")
                        .unwrap()
                        .into_pointer_value();
                    cur_ty = self.deref_target_type(cur_ty);
                }
            }
        }

        ptr
    }

    fn place_type(&self, place: &Place, func: &MirFunction) -> TypeId {
        let mut ty = func.local(place.local).ty;
        for elem in &place.projection {
            match elem {
                PlaceElem::Field(field_def) => ty = self.field_index_and_type(ty, *field_def).1,
                PlaceElem::Index(_) => ty = self.index_element_type(ty),
                PlaceElem::Deref => ty = self.deref_target_type(ty),
            }
        }
        ty
    }

    fn field_index_and_type(&self, base: TypeId, field_def: DefId) -> (u32, TypeId) {
        let layout = &self.program.struct_layouts[&base];
        let index = layout
            .fields
            .iter()
            .position(|field| field.def_id == field_def)
            .expect("field must be present in struct layout");
        (index as u32, layout.fields[index].ty)
    }

    fn index_element_type(&self, ty: TypeId) -> TypeId {
        match self.typecheck.interner.get(ty).clone() {
            Type::Array { element, .. } | Type::Slice { element, .. } => element,
            Type::ManyPointer { inner, .. } => inner,
            _ => panic!("indexing a non-indexable type"),
        }
    }

    fn deref_target_type(&self, ty: TypeId) -> TypeId {
        match self.typecheck.interner.get(ty).clone() {
            Type::Pointer { inner, .. } | Type::ManyPointer { inner, .. } => inner,
            _ => panic!("dereferencing a non-pointer type"),
        }
    }

    fn get_or_declare_runtime_fn(
        &mut self,
        name: &str,
        ret: AnyTypeEnum<'ctx>,
        params: &[BasicMetadataTypeEnum<'ctx>],
        is_var_args: bool,
    ) -> FunctionValue<'ctx> {
        if let Some(function) = self.module.get_function(name) {
            return function;
        }
        let fn_type = self.make_fn_type(ret, params, is_var_args);
        self.module
            .add_function(name, fn_type, Some(inkwell::module::Linkage::External))
    }

    fn get_str_global(&mut self, content: &str) -> GlobalValue<'ctx> {
        if let Some(global) = self.strings.get(content) {
            return *global;
        }
        let name = format!("str.{}", self.str_counter);
        self.str_counter += 1;
        let global = self
            .builder
            .build_global_string_ptr(content, &name)
            .unwrap();
        self.strings.insert(content.to_string(), global);
        global
    }

    fn resolve_spur(&self, spur: Spur) -> String {
        self.rodeo.borrow().resolve(&spur).to_string()
    }

    fn function_symbol_name(&self, id: MirFunctionId, func: &MirFunction) -> String {
        if let Some(symbol) = self.program.extern_exports.get(&id) {
            return symbol.clone();
        }
        if func.is_drop_impl {
            return format!("zeen.drop.{}", id.0);
        }

        let readable = self
            .program
            .function_names
            .get(&id)
            .cloned()
            .unwrap_or_else(|| format!("fn{}", id.0));

        if readable == "main" {
            return "zeen_main".to_string();
        }

        self.mangle_function_name(&readable, id)
    }

    fn mangle_function_name(&self, readable: &str, id: MirFunctionId) -> String {
        let mut mangled = String::new();
        for ch in readable.chars() {
            match ch {
                '[' => mangled.push('$'),
                ']' => mangled.push('$'),
                ',' => mangled.push('_'),
                ' ' => mangled.push('_'),
                c => mangled.push(c),
            }
        }
        if self.module.get_function(&mangled).is_some() {
            mangled.push_str(&format!("${}", id.0));
        }
        mangled
    }

    fn mangle_struct_name(&self, ty: TypeId) -> String {
        match self.typecheck.interner.get(ty) {
            Type::Struct {
                def_id,
                generic_args,
            } => {
                let base = self.resolve_def_name(*def_id);
                if generic_args.is_empty() {
                    base
                } else {
                    let args: Vec<String> = generic_args
                        .iter()
                        .map(|&arg| self.mangle_type_name(arg))
                        .collect();
                    format!("{base}${}", args.join("$"))
                }
            }
            Type::Slice { element, .. } => format!("slice.{}", self.mangle_type_name(*element)),
            _ => format!("struct.{}", ty.0),
        }
    }

    fn mangle_type_name(&self, ty: TypeId) -> String {
        match self.typecheck.interner.get(ty) {
            Type::Builtin(b) => format!("{b:?}"),
            Type::Struct {
                def_id,
                generic_args,
            } => {
                let base = self.resolve_def_name(*def_id);
                if generic_args.is_empty() {
                    base
                } else {
                    let args: Vec<String> = generic_args
                        .iter()
                        .map(|&arg| self.mangle_type_name(arg))
                        .collect();
                    format!("{base}${}", args.join("$"))
                }
            }
            Type::Pointer { inner, .. } => format!("ptr.{}", self.mangle_type_name(*inner)),
            Type::ManyPointer { inner, .. } => format!("many.{}", self.mangle_type_name(*inner)),
            Type::Array { element, len } => {
                format!(
                    "arr{}.{}",
                    len.unwrap_or(0),
                    self.mangle_type_name(*element)
                )
            }
            Type::Slice { element, .. } => format!("slice.{}", self.mangle_type_name(*element)),
            Type::Enum { def_id } => self.resolve_def_name(*def_id),
            Type::Fn { .. } => "fn".to_string(),
            Type::IntLiteral => "i32".to_string(),
            Type::FloatLiteral => "f64".to_string(),
            _ => format!("ty{}", ty.0),
        }
    }

    fn resolve_def_name(&self, def_id: DefId) -> String {
        self.resolution
            .defs
            .get(&def_id)
            .map(|info| self.resolve_spur(info.name))
            .unwrap_or_else(|| format!("<def#{def_id:?}>"))
    }
}
