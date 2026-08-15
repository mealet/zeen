//! Test-only support: hand-built MIR fixtures.
//!
//! The frontend is a binary crate (`zeen`), so codegen tests cannot run the
//! real pipeline. Instead, tests construct a minimal
//! [`Fixture`] (`MirProgram` + `TypeCheckResult` + `ResolutionResult` +
//! `Rodeo`) with a small DSL, then feed it to [`crate::CodeGen`].
//!
//! Example:
//!
//! ```ignore
//! let mut fx = Fixture::new();
//! let main_def = fx.def("main", DefKind::Function);
//! let i32 = fx.i32();
//!
//! let mut main = fx.fn_builder("main", main_def, i32);
//! let ret = main.temp(i32);
//! main.entry("bb0");
//! main.assign(place(ret), Rvalue::Use(const_int(42)));
//! main.ret(copy_of(ret));
//! main.finish();
//! ```

#![allow(dead_code)]

use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

use lasso::Spur;
use miette::{NamedSource, SourceOffset, SourceSpan};
use zeen_ast::{
    Source,
    expressions::{BinaryOp, UnaryOp},
    types::BuiltinType,
};
use zeen_hir::HirMacroKind;
use zeen_mir::{
    BlockId, CallTarget, ConstValue, LocalDecl, LocalId, LocalKind, MirFunction, MirFunctionId,
    MirProgram, MirStatement, Mutability, Operand, Place, Rvalue, Terminator,
};
use zeen_resolve::{DefId, DefInfo, DefKind, ResolutionResult};
use zeen_typecheck::{format_str::FormatChunk, result::TypeCheckResult};
use zeen_types::{Type, TypeId};

pub struct Fixture {
    pub typecheck: TypeCheckResult,
    pub resolution: ResolutionResult,
    pub rodeo: Rc<RefCell<lasso::Rodeo>>,
    pub program: MirProgram,
    /// Set automatically when a function named `main` is built.
    pub main_fn: Option<MirFunctionId>,
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Fixture {
    pub fn new() -> Self {
        Self {
            typecheck: TypeCheckResult::default(),
            resolution: ResolutionResult::default(),
            rodeo: Rc::new(RefCell::new(lasso::Rodeo::default())),
            program: MirProgram::default(),
            main_fn: None,
        }
    }

    pub fn ty(&mut self, ty: Type) -> TypeId {
        self.typecheck.interner.intern(ty)
    }

    pub fn i8(&mut self) -> TypeId {
        self.ty(Type::Builtin(BuiltinType::i8))
    }
    pub fn i16(&mut self) -> TypeId {
        self.ty(Type::Builtin(BuiltinType::i16))
    }
    pub fn i32(&mut self) -> TypeId {
        self.ty(Type::Builtin(BuiltinType::i32))
    }
    pub fn i64(&mut self) -> TypeId {
        self.ty(Type::Builtin(BuiltinType::i64))
    }
    pub fn u8(&mut self) -> TypeId {
        self.ty(Type::Builtin(BuiltinType::u8))
    }
    pub fn u32(&mut self) -> TypeId {
        self.ty(Type::Builtin(BuiltinType::u32))
    }
    pub fn u64(&mut self) -> TypeId {
        self.ty(Type::Builtin(BuiltinType::u64))
    }
    pub fn isize(&mut self) -> TypeId {
        self.ty(Type::Builtin(BuiltinType::isize))
    }
    pub fn usize(&mut self) -> TypeId {
        self.ty(Type::Builtin(BuiltinType::usize))
    }
    pub fn f32(&mut self) -> TypeId {
        self.ty(Type::Builtin(BuiltinType::f32))
    }
    pub fn f64(&mut self) -> TypeId {
        self.ty(Type::Builtin(BuiltinType::f64))
    }
    pub fn bool(&mut self) -> TypeId {
        self.ty(Type::Builtin(BuiltinType::bool))
    }
    pub fn char(&mut self) -> TypeId {
        self.ty(Type::Builtin(BuiltinType::char))
    }
    pub fn void(&mut self) -> TypeId {
        self.ty(Type::Void)
    }

    pub fn ptr(&mut self, inner: TypeId) -> TypeId {
        self.ty(Type::Pointer {
            inner,
            is_const: false,
        })
    }

    pub fn array(&mut self, element: TypeId, len: u64) -> TypeId {
        self.ty(Type::Array {
            element,
            len: Some(len),
        })
    }

    pub fn slice(&mut self, element: TypeId) -> TypeId {
        self.ty(Type::Slice {
            element,
            is_const: false,
        })
    }

    pub fn intern(&mut self, value: &str) -> Spur {
        self.rodeo.borrow_mut().get_or_intern(value)
    }

    pub fn source(&self) -> Source {
        Source {
            span: SourceSpan::new(SourceOffset::from(0), 0),
            src: NamedSource::new("test.zn", Arc::new(String::new())),
        }
    }

    pub fn def(&mut self, name: &str, kind: DefKind) -> DefId {
        let id = DefId(self.resolution.defs.len() as u32);
        let name = self.intern(name);
        self.resolution.defs.insert(
            id,
            DefInfo {
                name,
                kind,
                span: self.source(),
                decl: None,
                is_pub: false,
            },
        );
        id
    }

    pub fn add_extern_fn(
        &mut self,
        symbol_name: &str,
        param_types: Vec<TypeId>,
        ret_ty: TypeId,
        is_variadic: bool,
    ) {
        self.program.extern_fns.push(zeen_mir::ExternFnDecl {
            symbol_name: symbol_name.to_string(),
            param_types,
            ret_ty,
            is_variadic,
        });
    }

    pub fn add_struct_layout(&mut self, ty: TypeId, layout: zeen_mir::StructLayout) {
        self.program.struct_layouts.insert(ty, layout);
    }

    pub fn fn_builder(&mut self, name: &str, source_def: DefId, ret_ty: TypeId) -> FnBuilder<'_> {
        FnBuilder {
            fixture: self,
            name: name.to_string(),
            func: MirFunction {
                source_def,
                mono_args: Vec::new(),
                locals: Vec::new(),
                blocks: Vec::new(),
                params: Vec::new(),
                entry_block: BlockId(0),
                ret_ty,
                is_drop_impl: false,
            },
            name_to_local: HashMap::new(),
            name_to_block: HashMap::new(),
            current: BlockId(0),
        }
    }
}

/// Builder for a single [`MirFunction`]. Locals/blocks are created in order,
/// the first block created is the entry block (`bb0`).
pub struct FnBuilder<'f> {
    fixture: &'f mut Fixture,
    name: String,
    func: MirFunction,
    name_to_local: HashMap<String, LocalId>,
    name_to_block: HashMap<String, BlockId>,
    current: BlockId,
}

impl<'f> FnBuilder<'f> {
    fn new_local(
        &mut self,
        name: Option<&str>,
        ty: TypeId,
        mutability: Mutability,
        kind: LocalKind,
    ) -> LocalId {
        let spur = name.map(|n| self.fixture.intern(n));
        let id = self.func.new_local(LocalDecl {
            ty,
            mutability,
            kind,
            name: spur,
            source: Some(self.fixture.source()),
        });
        if let Some(name) = name {
            self.name_to_local.insert(name.to_string(), id);
        }
        id
    }

    pub fn param(&mut self, name: &str, ty: TypeId) -> LocalId {
        let id = self.new_local(Some(name), ty, Mutability::Const, LocalKind::Param);
        self.func.params.push(id);
        id
    }

    pub fn local(&mut self, name: &str, ty: TypeId) -> LocalId {
        self.new_local(Some(name), ty, Mutability::Mut, LocalKind::UserVariable)
    }

    pub fn local_const(&mut self, name: &str, ty: TypeId) -> LocalId {
        self.new_local(Some(name), ty, Mutability::Const, LocalKind::UserVariable)
    }

    pub fn temp(&mut self, ty: TypeId) -> LocalId {
        self.new_local(None, ty, Mutability::Mut, LocalKind::Temporary)
    }

    pub fn local_by(&self, name: &str) -> LocalId {
        self.name_to_local[name]
    }

    pub fn entry(&mut self, name: &str) -> BlockId {
        assert!(
            self.func.blocks.is_empty(),
            "entry block must be created first"
        );
        self.block(name)
    }

    pub fn block(&mut self, name: &str) -> BlockId {
        let id = self.func.new_block();
        self.name_to_block.insert(name.to_string(), id);
        self.current = id;
        id
    }

    pub fn block_by(&self, name: &str) -> BlockId {
        self.name_to_block[name]
    }

    pub fn set_current(&mut self, name: &str) {
        self.current = self.block_by(name);
    }

    fn stmt(&mut self, statement: MirStatement) {
        self.func.block_mut(self.current).statements.push(statement);
    }

    fn set_terminator(&mut self, terminator: Terminator) {
        self.func.block_mut(self.current).terminator = terminator;
    }

    pub fn assign(&mut self, place: Place, rvalue: Rvalue) {
        self.stmt(MirStatement::Assign {
            place,
            rvalue,
            source: None,
        });
    }

    pub fn storage_live(&mut self, local: LocalId) {
        self.stmt(MirStatement::StorageLive(local));
    }

    pub fn storage_dead(&mut self, local: LocalId) {
        self.stmt(MirStatement::StorageDead(local));
    }

    pub fn discard(&mut self, operand: Operand) {
        self.stmt(MirStatement::Discard(operand));
    }

    pub fn ret(&mut self, operand: Operand) {
        self.set_terminator(Terminator::Return(operand));
    }

    pub fn ret_void(&mut self) {
        self.ret(Operand::Constant(ConstValue::Void, None));
    }

    pub fn goto(&mut self, target: &str) {
        self.set_terminator(Terminator::Goto(self.block_by(target)));
    }

    pub fn unreachable(&mut self) {
        self.set_terminator(Terminator::Unreachable);
    }

    pub fn call(
        &mut self,
        target: CallTarget,
        args: Vec<Operand>,
        destination: LocalId,
        next: Option<&str>,
    ) {
        let terminator = Terminator::Call {
            func: target,
            args,
            destination: Place::from_local(destination),
            target: next.map(|name| self.block_by(name)),
            source: None,
        };
        self.set_terminator(terminator);
    }

    pub fn panic(&mut self, message: &str, args: Vec<Operand>, destination: LocalId) {
        let terminator = Terminator::MacroCall {
            kind: HirMacroKind::Panic,
            format_chunks: Some(vec![FormatChunk::Literal(message.to_string())]),
            args,
            arg_types: Vec::new(),
            destination: Place::from_local(destination),
            target: None,
            source: None,
        };
        self.set_terminator(terminator);
    }

    pub fn print(
        &mut self,
        message: &str,
        args: Vec<Operand>,
        arg_types: Vec<TypeId>,
        destination: LocalId,
        next: Option<&str>,
    ) {
        let terminator = Terminator::MacroCall {
            kind: HirMacroKind::Print,
            format_chunks: Some(vec![FormatChunk::Literal(message.to_string())]),
            args,
            arg_types,
            destination: Place::from_local(destination),
            target: next.map(|name| self.block_by(name)),
            source: None,
        };
        self.set_terminator(terminator);
    }

    pub fn println(
        &mut self,
        message: &str,
        args: Vec<Operand>,
        arg_types: Vec<TypeId>,
        destination: LocalId,
        next: Option<&str>,
    ) {
        let terminator = Terminator::MacroCall {
            kind: HirMacroKind::Println,
            format_chunks: Some(vec![FormatChunk::Literal(message.to_string())]),
            args,
            arg_types,
            destination: Place::from_local(destination),
            target: next.map(|name| self.block_by(name)),
            source: None,
        };
        self.set_terminator(terminator);
    }

    pub fn format(
        &mut self,
        chunks: Vec<FormatChunk>,
        args: Vec<Operand>,
        arg_types: Vec<TypeId>,
        destination: LocalId,
        next: Option<&str>,
    ) {
        let terminator = Terminator::MacroCall {
            kind: HirMacroKind::Format,
            format_chunks: Some(chunks),
            args,
            arg_types,
            destination: Place::from_local(destination),
            target: next.map(|name| self.block_by(name)),
            source: None,
        };
        self.set_terminator(terminator);
    }

    pub fn finish(mut self) -> MirFunctionId {
        assert!(
            !self.func.blocks.is_empty(),
            "function must have at least an entry block"
        );
        self.func.entry_block = BlockId(0);

        let id = MirFunctionId(self.fixture.program.functions.len() as u32);
        self.fixture
            .program
            .function_names
            .insert(id, self.name.clone());
        self.fixture.program.functions.insert(id, self.func);

        if self.name == "main" {
            self.fixture.main_fn = Some(id);
        }
        id
    }
}

pub fn const_int(n: i128) -> Operand {
    Operand::Constant(ConstValue::Int(n), None)
}

pub fn const_bool(b: bool) -> Operand {
    Operand::Constant(ConstValue::Bool(b), None)
}

pub fn const_str(fixture: &mut Fixture, value: &str) -> Operand {
    Operand::Constant(ConstValue::Str(fixture.intern(value)), None)
}

pub fn place(local: LocalId) -> Place {
    Place::from_local(local)
}

pub fn copy_of(local: LocalId) -> Operand {
    Operand::Copy(Place::from_local(local), None)
}

pub fn move_of(local: LocalId) -> Operand {
    Operand::Move(Place::from_local(local), None)
}

pub fn use_local(local: LocalId) -> Rvalue {
    Rvalue::Use(copy_of(local))
}

pub fn use_const(operand: Operand) -> Rvalue {
    Rvalue::Use(operand)
}

pub fn binary(op: BinaryOp, lhs: Operand, rhs: Operand) -> Rvalue {
    Rvalue::BinaryOp { op, lhs, rhs }
}

pub fn unary(op: UnaryOp, operand: Operand) -> Rvalue {
    Rvalue::UnaryOp { op, operand }
}
