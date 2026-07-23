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

struct LoopTargets {
    break_target: BlockId,
    continue_target: BlockId,
}

pub struct FnBuilder {
    func: MirFunction,
    locals_by_def: HashMap<DefId, LocalId>,
    loop_stack: Vec<LoopTargets>,
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
            loop_stack: Vec::new(),
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
        match &expr.kind {
            HirExprKind::Literal(lit) => {
                let ty = self.expr_type(expr);
                (block, Operand::Constant(self.lower_literal(lit, ty)))
            }

            HirExprKind::VarRef(def_id) | HirExprKind::SelfValue(def_id) => {
                let local = *fb.locals_by_def.get(def_id).unwrap_or_else(|| {
                    panic!("HIR DefId {:?} has no MIR local", def_id);
                });
                let place = Place::from_local(local);
                let ty = fb.func.local(local).ty;
                let operand = self.place_to_operand(place, ty, &HashMap::new());
                (block, operand)
            }

            HirExprKind::Binary { lhs, rhs, op } => {
                let (block, lhs_op) = self.lower_expr_to_operand(fb, lhs, block);
                let (block, rhs_op) = self.lower_expr_to_operand(fb, rhs, block);

                let result_ty = self.expr_type(expr);
                let temp = fb.new_temp(result_ty);

                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place: Place::from_local(temp),
                        rvalue: Rvalue::BinaryOp {
                            op: *op,
                            lhs: lhs_op,
                            rhs: rhs_op,
                        },
                    },
                );

                (block, Operand::Move(Place::from_local(temp)))
            }

            HirExprKind::Unary { expr: inner, op } => {
                let (block, inner_op) = self.lower_expr_to_operand(fb, inner, block);

                let result_ty = self.expr_type(expr);
                let temp = fb.new_temp(result_ty);

                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place: Place::from_local(temp),
                        rvalue: Rvalue::UnaryOp {
                            op: *op,
                            operand: inner_op,
                        },
                    },
                );

                (block, Operand::Move(Place::from_local(temp)))
            }

            HirExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let (block, cond_operand) = self.lower_expr_to_operand(fb, condition, block);

                let then_bb = fb.new_block();
                let else_bb = fb.new_block();

                fb.set_terminator(
                    block,
                    Terminator::SwitchInt {
                        discrimant: cond_operand,
                        targets: vec![(1, then_bb)],
                        otherwise: else_bb,
                    },
                );

                let (then_end, then_operand) =
                    self.lower_stmt_as_block_value(fb, then_block, then_bb);

                let result_ty = self.expr_type(expr);
                let has_else = else_block.is_some();

                if !has_else {
                    let join = fb.new_block();
                    fb.set_terminator(then_end, Terminator::Goto(join));
                    fb.set_terminator(else_bb, Terminator::Goto(join));
                    return (join, Operand::Constant(ConstValue::Void));
                }

                let (else_end, else_operand) =
                    self.lower_stmt_as_block_value(fb, else_block.as_ref().unwrap(), else_bb);
                let join = fb.new_block();

                let result_local = fb.new_temp(result_ty);

                fb.push_stmt(
                    then_end,
                    MirStatement::Assign {
                        place: Place::from_local(result_local),
                        rvalue: Rvalue::Use(then_operand),
                    },
                );
                fb.set_terminator(then_end, Terminator::Goto(join));

                fb.push_stmt(
                    else_end,
                    MirStatement::Assign {
                        place: Place::from_local(result_local),
                        rvalue: Rvalue::Use(else_operand),
                    },
                );
                fb.set_terminator(else_end, Terminator::Goto(join));

                (join, Operand::Move(Place::from_local(result_local)))
            }

            HirExprKind::Block { stmts, trailing } => {
                let mut cur = block;

                for stmt in stmts.iter() {
                    cur = self.lower_stmt(fb, stmt, cur);
                }

                match trailing {
                    Some(t) => self.lower_expr_to_operand(fb, t, cur),
                    None => (cur, Operand::Constant(ConstValue::Void)),
                }
            }

            _ => todo!(),
        }
    }

    fn lower_expr_to_place(
        &mut self,
        fb: &mut FnBuilder,
        expr: &HirExpr,
        block: BlockId,
    ) -> (BlockId, Place) {
        match &expr.kind {
            HirExprKind::VarRef(def_id) | HirExprKind::SelfValue(def_id) => {
                let local = *fb.locals_by_def.get(def_id).expect("undeclared local");
                (block, Place::from_local(local))
            }

            _ => todo!(),
        }
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

impl<'ctx> MirLowering<'ctx> {
    fn lower_stmt_as_block_value(
        &mut self,
        fb: &mut FnBuilder,
        stmt: &HirStmt,
        block: BlockId,
    ) -> (BlockId, Operand) {
        match &stmt.kind {
            HirStmtKind::Expr(block_expr) => self.lower_expr_to_operand(fb, block_expr, block),
            _ => {
                let block = self.lower_stmt(fb, stmt, block);
                (block, Operand::Constant(ConstValue::Void))
            }
        }
    }

    fn lower_stmt(&mut self, fb: &mut FnBuilder, stmt: &HirStmt, block: BlockId) -> BlockId {
        match &stmt.kind {
            HirStmtKind::Let {
                name,
                def_id,
                value,
                ..
            } => {
                let ty = self
                    .expr_types
                    .get(&stmt.id)
                    .copied()
                    .unwrap_or_else(|| panic!("let statement missing recorded type"));

                let local = fb.new_local(
                    ty,
                    LocalKind::UserVariable,
                    Mutability::Mut,
                    Some(*name),
                    Some(stmt.source.clone()),
                );
                fb.locals_by_def.insert(*def_id, local);

                if let Some(v) = value {
                    let (block, operand) = self.lower_expr_to_operand(fb, v, block);

                    fb.push_stmt(
                        block,
                        MirStatement::Assign {
                            place: Place::from_local(local),
                            rvalue: Rvalue::Use(operand),
                        },
                    );
                    block
                } else {
                    block
                }
            }

            HirStmtKind::While {
                condition,
                block: body,
            } => {
                let header = fb.new_block();
                fb.set_terminator(block, Terminator::Goto(header));

                let (cond_end, cond_operand) = self.lower_expr_to_operand(fb, condition, header);

                let body_bb = fb.new_block();
                let exit_bb = fb.new_block();

                fb.set_terminator(
                    cond_end,
                    Terminator::SwitchInt {
                        discrimant: cond_operand,
                        targets: vec![(1, body_bb)],
                        otherwise: exit_bb,
                    },
                );

                fb.loop_stack.push(LoopTargets {
                    break_target: exit_bb,
                    continue_target: header,
                });
                let body_end = self.lower_stmt_as_block_value(fb, body, body_bb).0;
                fb.loop_stack.pop();

                fb.set_terminator(body_end, Terminator::Goto(header));

                exit_bb
            }

            HirStmtKind::Return { value } => {
                let operand = match value {
                    Some(v) => {
                        let (b, op) = self.lower_expr_to_operand(fb, v, block);
                        let block = b;
                        fb.set_terminator(block, Terminator::Return(op));
                        return block;
                    }
                    None => Operand::Constant(ConstValue::Void),
                };
                fb.set_terminator(block, Terminator::Return(operand));
                block
            }

            HirStmtKind::Break => {
                let target = fb
                    .loop_stack
                    .last()
                    .expect("break outside loop not covered")
                    .break_target;
                fb.set_terminator(block, Terminator::Goto(target));
                block
            }

            HirStmtKind::Continue => {
                let target = fb
                    .loop_stack
                    .last()
                    .expect("continue outside loop not covered")
                    .continue_target;
                fb.set_terminator(block, Terminator::Goto(target));
                block
            }

            HirStmtKind::Expr(expr) => {
                let (block, _operand) = self.lower_expr_to_operand(fb, expr, block);
                block
            }

            _ => todo!(),
        }
    }
}
