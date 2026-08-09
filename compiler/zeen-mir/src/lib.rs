// TODO: Remove `unused` config in working version to remove unnecessary code.
#![allow(unused)]

use std::collections::HashMap;

use lasso::Spur;
use zeen_ast::{
    Source,
    expressions::{BinaryOp, UnaryOp},
};
use zeen_hir::HirMacroKind;
use zeen_resolve::DefId;
use zeen_typecheck::format_str::FormatChunk;
use zeen_types::TypeId;

pub mod collecter;
pub mod lowering;
pub mod printer;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MirFunctionId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

#[derive(Debug, Default)]
pub struct MirProgram {
    pub functions: HashMap<MirFunctionId, MirFunction>,
    pub function_names: HashMap<MirFunctionId, String>,
    pub struct_layouts: HashMap<TypeId, StructLayout>,

    pub extern_fns: Vec<ExternFnDecl>,
    pub extern_exports: HashMap<MirFunctionId, String>,
    pub extern_vars: Vec<ExternVarDecl>,
}

#[derive(Debug, Clone)]
pub struct ExternFnDecl {
    pub symbol_name: String,
    pub param_types: Vec<TypeId>,
    pub ret_ty: TypeId,
    pub is_variadic: bool,
}

#[derive(Debug, Clone)]
pub struct ExternVarDecl {
    pub symbol_name: String,
    pub ty: TypeId,
}

#[derive(Debug, Clone)]
pub struct StructLayout {
    pub def_id: DefId,
    pub generic_args: Vec<TypeId>,
    pub fields: Vec<StructFieldLayout>,
}

#[derive(Debug, Clone)]
pub struct StructFieldLayout {
    pub def_id: DefId,
    pub ty: TypeId,
}

#[derive(Debug)]
pub struct MirFunction {
    pub source_def: DefId,
    pub mono_args: Vec<TypeId>,

    pub locals: Vec<LocalDecl>,
    pub blocks: Vec<BasicBlock>,
    pub params: Vec<LocalId>,

    pub entry_block: BlockId,
    pub ret_ty: TypeId,

    /// Whether this function is the generated `drop` implementation of a
    /// struct, produced by [`lowering::register_drop_functions`]. Its `self`
    /// parameter must not get an automatic scope-exit drop.
    pub is_drop_impl: bool,
}

impl MirFunction {
    pub fn local(&self, id: LocalId) -> &LocalDecl {
        &self.locals[id.0 as usize]
    }

    pub fn local_mut(&mut self, id: LocalId) -> &mut LocalDecl {
        &mut self.locals[id.0 as usize]
    }

    pub fn block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.0 as usize]
    }

    pub fn block_mut(&mut self, id: BlockId) -> &mut BasicBlock {
        &mut self.blocks[id.0 as usize]
    }

    pub fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(BasicBlock {
            statements: Vec::new(),
            terminator: Terminator::Unreachable, // placeholder
        });
        id
    }

    pub fn new_local(&mut self, decl: LocalDecl) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(decl);
        id
    }
}

#[derive(Debug, Clone)]
pub struct LocalDecl {
    pub ty: TypeId,
    pub mutability: Mutability,
    pub kind: LocalKind,
    pub name: Option<Spur>,
    pub source: Option<Source>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutability {
    Mut,
    Const,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    Param,
    UserVariable,
    Temporary,
    ReturnSlot,
}

#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub statements: Vec<MirStatement>,
    pub terminator: Terminator,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum MirStatement {
    Assign {
        place: Place,
        rvalue: Rvalue,
        /// Source of the expression that produced this statement, used for
        /// diagnostics on reads of the operands.
        source: Option<Source>,
    },
    Drop(Place),

    /// Evaluates an operand and throws the value away, e.g. `let _ = expr;`.
    /// The operand is still consumed (moves are recorded), but no local is
    /// allocated and nothing is stored.
    Discard(Operand),

    StorageLive(LocalId),
    StorageDead(LocalId),

    Nop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub local: LocalId,
    pub projection: Vec<PlaceElem>,
}

impl Place {
    pub fn from_local(local: LocalId) -> Self {
        Self {
            local,
            projection: Vec::new(),
        }
    }

    pub fn field(mut self, field_def: DefId) -> Self {
        self.projection.push(PlaceElem::Field(field_def));
        self
    }

    pub fn deref(mut self) -> Self {
        self.projection.push(PlaceElem::Deref);
        self
    }

    pub fn index(mut self, index_local: LocalId) -> Self {
        self.projection.push(PlaceElem::Index(index_local));
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaceElem {
    Field(DefId),
    Index(LocalId),
    Deref,

    // builtin slice's fields
    SliceLen,
    SlicePtr,
}

#[derive(Debug, Clone)]
pub enum Operand {
    Copy(Place, Option<Source>),
    Move(Place, Option<Source>),
    Constant(ConstValue, Option<Source>),
}

#[derive(Debug, Clone)]
pub enum ConstValue {
    Int(i128),
    Float(f64),
    Bool(bool),
    Char(char),
    Str(Spur),
    NullPtr,
    Void,
}

#[derive(Debug, Clone)]
pub enum Rvalue {
    Use(Operand),

    BinaryOp {
        op: BinaryOp,
        lhs: Operand,
        rhs: Operand,
    },

    UnaryOp {
        op: UnaryOp,
        operand: Operand,
    },

    Ref {
        place: Place,
        is_const: bool,
    },

    Cast {
        operand: Operand,
        target: TypeId,
    },

    SizeOf(TypeId),
    AlignOf(TypeId),

    Aggregate {
        kind: AggregateKind,
        operands: Vec<Operand>,
    },

    Discriminant(Place),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateKind {
    Struct(DefId),
    Array,
    Slice,
}

#[derive(Debug, Clone)]
pub enum Terminator {
    Goto(BlockId),

    SwitchInt {
        discriminant: Operand,
        targets: Vec<(u128, BlockId)>,
        otherwise: BlockId,
    },

    Call {
        func: CallTarget,
        args: Vec<Operand>,
        destination: Place,
        target: Option<BlockId>,
        /// Source of the call expression, used for diagnostics on arg reads.
        source: Option<Source>,
    },

    MacroCall {
        kind: HirMacroKind,
        format_chunks: Option<Vec<FormatChunk>>,
        args: Vec<Operand>,
        destination: Place,
        target: Option<BlockId>,
        /// Source of the macro call expression, used for diagnostics on arg reads.
        source: Option<Source>,
    },

    Return(Operand),

    Unreachable,
}

#[derive(Debug, Clone)]
pub enum CallTarget {
    /// Statically known function
    Direct(MirFunctionId),
    /// Call through a function-pointer value
    Indirect(Operand),
    /// Call to declared extern function. Index into `MirProgram.extern_fns`
    Extern(usize),
}
