use std::{collections::HashMap, rc::Rc};

use lasso::Spur;
use zeen_ast::{
    Source,
    expressions::{BinaryOp, Literal, UnaryOp},
};
use zeen_hir::{
    HirId,
    decl::HirFn,
    expr::{HirExpr, HirExprKind},
    stmt::{HirStmt, HirStmtKind},
};
use zeen_resolve::DefId;
use zeen_types::{StructTypeInfo, Type, TypeId, TypeInterner};

use crate::{
    AggregateKind, BasicBlock, BlockId, CallTarget, ConstValue, LocalDecl, LocalId, LocalKind,
    MirFunction, MirFunctionId, MirProgram, MirStatement, Mutability, Operand, Place, PlaceElem,
    Rvalue, Terminator,
};

pub struct MirLowering<'ctx> {
    interner: &'ctx mut TypeInterner,
    expr_types: &'ctx HashMap<HirId, TypeId>,

    program: MirProgram,
    mono_cache: MonoCache,
}

pub struct MonoCache {
    cache: HashMap<(DefId, Vec<TypeId>), MirFunctionId>,
    next_id: u32,
}

impl MonoCache {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            next_id: 0,
        }
    }

    fn fresh_id(&mut self) -> MirFunctionId {
        let id = MirFunctionId(self.next_id);
        self.next_id += 1;
        id
    }
}

pub struct FnBuilder {
    func: MirFunction,
    locals_by_def: HashMap<DefId, LocalId>,
}

impl FnBuilder {
    pub fn new(source_def: DefId, mono_args: Vec<TypeId>, entry: BlockId) -> Self {
        Self {
            func: MirFunction {
                source_def,
                mono_args,
                locals: Vec::new(),
                blocks: Vec::new(),
                params: Vec::new(),
                entry_block: entry,
            },
            locals_by_def: HashMap::new(),
        }
    }

    fn new_local(
        &mut self,
        ty: TypeId,
        kind: LocalKind,
        mutability: Mutability,
        name: Option<Spur>,
        source: Option<Source>,
    ) -> LocalId {
        self.func.new_local(LocalDecl {
            ty,
            mutability,
            kind,
            name,
            source,
        })
    }

    fn new_temp(&mut self, ty: TypeId) -> LocalId {
        self.new_local(ty, LocalKind::Temporary, Mutability::Mut, None, None)
    }

    fn new_block(&mut self) -> BlockId {
        self.func.new_block()
    }

    fn push_stmt(&mut self, block: BlockId, stmt: MirStatement) {
        self.func.block_mut(block).statements.push(stmt);
    }

    fn set_terminator(&mut self, block: BlockId, term: Terminator) {
        self.func.block_mut(block).terminator = term;
    }
}

impl<'ctx> MirLowering<'ctx> {
    pub fn new(interner: &'ctx mut TypeInterner, expr_types: &'ctx HashMap<HirId, TypeId>) -> Self {
        Self {
            interner,
            expr_types,
            program: MirProgram::default(),
            mono_cache: MonoCache::new(),
        }
    }

    pub fn finish(self) -> MirProgram {
        self.program
    }

    fn expr_type(&self, expr: &HirExpr) -> TypeId {
        self.expr_types
            .get(&expr.id)
            .copied()
            .expect("unrecorded HIR expr after Typechecker")
    }

    fn mir_type_is_copy(&self, ty: TypeId, struct_info: &HashMap<DefId, StructTypeInfo>) -> bool {
        match self.interner.get(ty).clone() {
            Type::Builtin(_)
            | Type::Enum { .. }
            | Type::Pointer { .. }
            | Type::ManyPointer { .. }
            | Type::Fn { .. }
            | Type::Void
            | Type::Never
            | Type::Error => true,

            Type::Struct { def_id, .. } => struct_info
                .get(&def_id)
                .map(|info| info.capabalities.is_copy)
                .unwrap_or(false),

            Type::Slice { .. } | Type::Array { .. } => false,

            _ => false,
        }
    }
}

impl<'ctx> MirLowering<'ctx> {
    fn lower_expr_to_operand(
        &mut self,
        fb: &mut FnBuilder,
        expr: &HirExpr,
        block: BlockId,
    ) -> (BlockId, Operand) {
        todo!()
    }

    fn lower_literal(&mut self, lit: &Literal, ty: TypeId) -> ConstValue {
        match lit {
            Literal::Int(n) => ConstValue::Int(*n as i128),
            Literal::Float(f) => ConstValue::Float(*f),
            Literal::Bool(b) => ConstValue::Bool(*b),
            Literal::Char(c) | Literal::ByteChar(c) => ConstValue::Char(*c),
            Literal::String(s) => ConstValue::Str(*s),
            Literal::Null => ConstValue::NullPtr,
        }
    }

    fn place_to_operand(
        &self,
        place: Place,
        ty: TypeId,
        struct_info: &HashMap<DefId, StructTypeInfo>,
    ) -> Operand {
        if self.mir_type_is_copy(ty, struct_info) {
            Operand::Copy(place)
        } else {
            Operand::Move(place)
        }
    }
}
