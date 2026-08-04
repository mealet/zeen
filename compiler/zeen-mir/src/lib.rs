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
pub enum MirStatement {
    Assign { place: Place, rvalue: Rvalue },
    Drop(Place),

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
    Copy(Place),
    Move(Place),
    Constant(ConstValue),
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
    },

    MacroCall {
        kind: HirMacroKind,
        format_chunks: Option<Vec<FormatChunk>>,
        args: Vec<Operand>,
        destination: Place,
        target: Option<BlockId>,
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
}
