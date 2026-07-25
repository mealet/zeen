use std::{collections::HashMap, rc::Rc};

use lasso::Spur;
use zeen_ast::{
    Source,
    expressions::{BinaryOp, Literal, UnaryOp},
};
use zeen_hir::{
    HirId, HirTypeExpr,
    decl::HirFn,
    expr::{HirExpr, HirExprKind},
    stmt::{HirStmt, HirStmtKind},
};
use zeen_resolve::{DefId, ResolutionResult};
use zeen_typecheck::result::{CallResolution, TypeCheckResult};
use zeen_types::{StructTypeInfo, Type, TypeId, TypeInterner};

use crate::{
    AggregateKind, BasicBlock, BlockId, CallTarget, ConstValue, LocalDecl, LocalId, LocalKind,
    MirFunction, MirFunctionId, MirProgram, MirStatement, Mutability, Operand, Place, PlaceElem,
    Rvalue, Terminator,
};

pub struct MirLowering<'ctx> {
    typecheck: &'ctx mut TypeCheckResult,
    resolution: &'ctx ResolutionResult,
    hir_fns_by_def: HashMap<DefId, Rc<HirFn>>,

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
    pub fn new(
        typecheck: &'ctx mut TypeCheckResult,
        resolution: &'ctx ResolutionResult,
        hir_fns_by_def: &'ctx HashMap<DefId, Rc<HirFn>>,
    ) -> Self {
        Self {
            typecheck,
            resolution,
            program: MirProgram::default(),
            mono_cache: MonoCache::new(),
            hir_fns_by_def: HashMap::new(),
        }
    }

    pub fn finish(self) -> MirProgram {
        self.program
    }

    fn expr_type(&self, expr: &HirExpr) -> TypeId {
        self.typecheck
            .expr_types
            .get(&expr.id)
            .copied()
            .expect("unrecorded HIR expr after Typechecker")
    }

    fn struct_info(&self, def_id: DefId) -> Option<&zeen_types::StructTypeInfo> {
        self.typecheck.struct_info.get(&def_id)
    }

    fn field_resolution(&self, expr_id: HirId) -> Option<DefId> {
        self.typecheck.field_resolutions.get(&expr_id).copied()
    }

    fn call_resolution(&self, expr_id: HirId) -> Option<&CallResolution> {
        self.typecheck.call_resolutions.get(&expr_id)
    }

    fn mir_type_is_copy(&self, ty: TypeId) -> bool {
        match self.typecheck.interner.get(ty).clone() {
            Type::Builtin(_)
            | Type::Enum { .. }
            | Type::Pointer { .. }
            | Type::ManyPointer { .. }
            | Type::Fn { .. }
            | Type::Void
            | Type::Never
            | Type::Error => true,

            Type::Struct { def_id, .. } => self
                .typecheck
                .struct_info
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
        mut block: BlockId,
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
                let operand = self.place_to_operand(place, ty);
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

            HirExprKind::StructInit { fields, .. } => {
                let ty = self.expr_type(expr);
                let struct_def = match self.typecheck.interner.get(ty).clone() {
                    Type::Struct { def_id, .. } => def_id,
                    _ => panic!("non-struct type in StructInit lowering"),
                };

                let info = self
                    .struct_info(struct_def)
                    .expect("struct info is missing")
                    .clone();

                let mut block = block;
                let mut ordered_operands = Vec::with_capacity(info.fields.len());

                for field_info in &info.fields {
                    let matching = fields
                        .iter()
                        .find(|f| f.name == field_info.name)
                        .expect("typechecker should have caught missing field");

                    let (bl, operand) = self.lower_expr_to_operand(fb, &matching.value, block);
                    block = bl;
                    ordered_operands.push(operand);
                }

                let temp = fb.new_temp(ty);

                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place: Place::from_local(temp),
                        rvalue: Rvalue::Aggregate {
                            kind: AggregateKind::Struct(struct_def),
                            operands: ordered_operands,
                        },
                    },
                );

                (block, self.place_to_operand(Place::from_local(temp), ty))
            }

            HirExprKind::ArrayInit { elements } => {
                let ty = self.expr_type(expr);
                let mut block = block;
                let mut operands = Vec::with_capacity(elements.len());

                for el in elements.iter() {
                    let (b, op) = self.lower_expr_to_operand(fb, el, block);
                    block = b;
                    operands.push(op);
                }

                let temp = fb.new_temp(ty);
                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place: Place::from_local(temp),
                        rvalue: Rvalue::Aggregate {
                            kind: AggregateKind::Array,
                            operands,
                        },
                    },
                );

                (block, self.place_to_operand(Place::from_local(temp), ty))
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

            HirExprKind::Call { callee, args, .. } => {
                let call_id = expr.id;

                let Some(resolution) = self.call_resolution(call_id) else {
                    return self.lower_indirect_call(fb, callee, args, block, self.expr_type(expr));
                };

                let fn_def = resolution.fn_def;
                let generic_args = resolution.generic_args.clone();

                let Some(hir_fn) = self.hir_fns_by_def.get(&fn_def).cloned() else {
                    panic!("No HIR Body found for DefId {:?}", fn_def);
                };

                let mir_fn_id = self.monomorphize_fn(fn_def, generic_args, &hir_fn);

                let mut arg_operands = Vec::with_capacity(args.len() + 1);

                if let HirExprKind::FieldAccess { object, .. } = &callee.kind {
                    let (b, self_operand) = self.lower_receiver_operand(fb, object, block);
                    block = b;
                    arg_operands.push(self_operand);
                }

                let ret_ty = self.expr_type(expr);
                let dest_local = fb.new_temp(ret_ty);
                let dest_place = Place::from_local(dest_local);

                let next_block = fb.new_block();
                let is_diverging = matches!(self.typecheck.interner.get(ret_ty), Type::Never);

                fb.set_terminator(
                    block,
                    Terminator::Call {
                        func: CallTarget::Direct(mir_fn_id),
                        args: arg_operands,
                        destination: dest_place.clone(),
                        target: if is_diverging { None } else { Some(next_block) },
                    },
                );

                if is_diverging {
                    fb.set_terminator(next_block, Terminator::Unreachable);
                    (next_block, Operand::Constant(ConstValue::Void))
                } else {
                    (next_block, self.place_to_operand(dest_place, ret_ty))
                }
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

            HirExprKind::FieldAccess { object, .. } => {
                let field_def = *self
                    .typecheck
                    .field_resolutions
                    .get(&expr.id)
                    .expect("unresolved shit");
                let (block, obj_place) = self.lower_expr_to_place(fb, object, block);
                (block, obj_place.field(field_def))
            }

            HirExprKind::SliceAccess { object, index } => {
                let (block, obj_place) = self.lower_expr_to_place(fb, object, block);
                let (block, index_operand) = self.lower_expr_to_operand(fb, index, block);

                let index_local = match index_operand {
                    Operand::Copy(p) | Operand::Move(p) if p.projection.is_empty() => p.local,
                    other => {
                        let usize_ty = self
                            .typecheck
                            .interner
                            .intern(Type::Builtin(zeen_ast::types::BuiltinType::usize));

                        let temp = fb.new_temp(usize_ty);

                        fb.push_stmt(
                            block,
                            MirStatement::Assign {
                                place: Place::from_local(temp),
                                rvalue: Rvalue::Use(other),
                            },
                        );

                        temp
                    }
                };

                (block, obj_place.index(index_local))
            }

            HirExprKind::Unary {
                expr: inner,
                op: UnaryOp::Deref,
            } => {
                let (block, inner_place) = self.lower_expr_to_place(fb, inner, block);
                (block, inner_place.deref())
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

    fn place_to_operand(&self, place: Place, ty: TypeId) -> Operand {
        if self.mir_type_is_copy(ty) {
            Operand::Copy(place)
        } else {
            Operand::Move(place)
        }
    }

    fn place_type(&self, fb: &FnBuilder, place: &Place) -> TypeId {
        fb.func.local(place.local).ty
    }

    fn lower_receiver_operand(
        &mut self,
        fb: &mut FnBuilder,
        object: &HirExpr,
        block: BlockId,
    ) -> (BlockId, Operand) {
        let obj_ty = self.expr_type(object);
        let (block, place) = self.lower_expr_to_place(fb, object, block);

        match self.typecheck.interner.get(obj_ty).clone() {
            Type::Pointer { .. } => (block, self.place_to_operand(place, obj_ty)),
            _ => {
                let ptr_ty = self.typecheck.interner.intern(Type::Pointer {
                    inner: obj_ty,
                    is_const: false,
                });
                let temp = fb.new_temp(ptr_ty);
                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place: Place::from_local(temp),
                        rvalue: Rvalue::Ref { place },
                    },
                );
                (block, Operand::Move(Place::from_local(temp)))
            }
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
                    .typecheck
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

            HirStmtKind::Assign { object, value } => {
                let (block, place) = self.lower_expr_to_place(fb, object, block);
                let (block, operand) = self.lower_expr_to_operand(fb, value, block);

                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place,
                        rvalue: Rvalue::Use(operand),
                    },
                );
                block
            }

            HirStmtKind::CompoundAssign { object, value, op } => {
                let (block, place) = self.lower_expr_to_place(fb, object, block);

                let place_ty = self.place_type(fb, &place);
                let lhs_operand = self.place_to_operand(place.clone(), place_ty);

                let (block, rhs_operand) = self.lower_expr_to_operand(fb, value, block);

                let result_ty = place_ty;
                let temp = fb.new_temp(result_ty);

                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place: Place::from_local(temp),
                        rvalue: Rvalue::BinaryOp {
                            op: *op,
                            lhs: lhs_operand,
                            rhs: rhs_operand,
                        },
                    },
                );
                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place,
                        rvalue: Rvalue::Use(Operand::Move(Place::from_local(temp))),
                    },
                );

                block
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

impl<'ctx> MirLowering<'ctx> {
    fn monomorphize_fn(
        &mut self,
        def_id: DefId,
        generic_args: Vec<TypeId>,
        hir_fn: &HirFn,
    ) -> MirFunctionId {
        let key = (def_id, generic_args.clone());

        if let Some(&existing) = self.mono_cache.cache.get(&key) {
            return existing;
        }

        let id = self.mono_cache.fresh_id();
        self.mono_cache.cache.insert(key, id);

        let mir_func = self.lower_fn_body(def_id, hir_fn, &generic_args);
        self.program.functions.insert(id, mir_func);

        id
    }

    fn lower_fn_body(
        &mut self,
        def_id: DefId,
        hir_fn: &HirFn,
        generic_args: &[TypeId],
    ) -> MirFunction {
        let generic_defs: Vec<DefId> = hir_fn.generics.iter().map(|g| g.def_id).collect();
        let bindings: HashMap<DefId, TypeId> = generic_defs
            .iter()
            .copied()
            .zip(generic_args.iter().copied())
            .collect();

        let entry = BlockId(0);
        let mut fb = FnBuilder::new(def_id, generic_args.to_vec(), entry);
        fb.new_block();

        for param in &hir_fn.params {
            let Some(param_def) = param.def_id else {
                continue;
            };

            let raw_ty = self
                .typecheck
                .def_types
                .get(&param_def)
                .copied()
                .expect("param must have a type after Typecheck");
            let concrete_ty =
                zeen_types::substitute_generics(&mut self.typecheck.interner, raw_ty, &bindings);

            let local = fb.new_local(
                concrete_ty,
                LocalKind::Param,
                Mutability::Mut,
                param.name,
                Some(param.ty.source.clone()),
            );

            fb.func.params.push(local);
            fb.locals_by_def.insert(param_def, local);
        }

        let Some(body) = &hir_fn.body else {
            fb.set_terminator(entry, Terminator::Unreachable);
            return fb.func;
        };

        let final_block = match &body.kind {
            HirStmtKind::Expr(block_expr) => {
                if let HirExprKind::Block { stmts, trailing } = &block_expr.kind {
                    let mut cur = entry;

                    for stmt in stmts.iter() {
                        cur = self.lower_stmt(&mut fb, stmt, cur);
                    }

                    match trailing {
                        Some(t) => {
                            let (block, operand) = self.lower_expr_to_operand(&mut fb, t, cur);

                            if matches!(fb.func.block(block).terminator, Terminator::Unreachable) {
                                fb.set_terminator(block, Terminator::Return(operand));
                            };
                            block
                        }

                        None => {
                            if matches!(fb.func.block(cur).terminator, Terminator::Unreachable) {
                                fb.set_terminator(
                                    cur,
                                    Terminator::Return(Operand::Constant(ConstValue::Void)),
                                );
                            }
                            cur
                        }
                    }
                } else {
                    let cur = self.lower_stmt(&mut fb, body, entry);
                    if matches!(fb.func.block(cur).terminator, Terminator::Unreachable) {
                        fb.set_terminator(
                            cur,
                            Terminator::Return(Operand::Constant(ConstValue::Void)),
                        );
                    }
                    cur
                }
            }

            _ => {
                let cur = self.lower_stmt(&mut fb, body, entry);
                if matches!(fb.func.block(cur).terminator, Terminator::Unreachable) {
                    fb.set_terminator(cur, Terminator::Return(Operand::Constant(ConstValue::Void)));
                }
                cur
            }
        };

        let _ = final_block;

        fb.func
    }

    fn lower_indirect_call(
        &mut self,
        fb: &mut FnBuilder,
        callee: &HirExpr,
        args: &[Rc<HirExpr>],
        block: BlockId,
        expr_type: TypeId,
    ) -> (BlockId, Operand) {
        todo!()
    }
}
