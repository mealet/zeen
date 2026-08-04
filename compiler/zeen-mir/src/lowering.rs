use std::{cell::RefCell, collections::HashMap, rc::Rc};

use lasso::{Rodeo, Spur};
use zeen_ast::{
    Source,
    expressions::{BinaryOp, Literal, UnaryOp},
};
use zeen_hir::{
    HirId, HirMacroKind, HirModule, HirTypeExpr,
    decl::HirFn,
    expr::{HirExpr, HirExprKind},
    stmt::{HirStmt, HirStmtKind},
};
use zeen_resolve::{DefId, ResolutionResult};
use zeen_typecheck::{
    coerce::builtin_is_integer,
    result::{CallResolution, OperatorResolution, TypeCheckResult},
};
use zeen_types::{StructTypeInfo, Type, TypeId, TypeInterner};

use crate::{
    AggregateKind, BasicBlock, BlockId, CallTarget, ConstValue, ExternFnDecl, LocalDecl, LocalId,
    LocalKind, MirFunction, MirFunctionId, MirProgram, MirStatement, Mutability, Operand, Place,
    PlaceElem, Rvalue, Terminator,
};

pub struct MirLoweringResult {
    pub program: MirProgram,
    pub main_fn: Option<MirFunctionId>,
}

pub fn lower_program<'ctx>(
    rodeo: Rc<RefCell<Rodeo>>,
    typecheck: &'ctx mut TypeCheckResult,
    resolution: &'ctx ResolutionResult,
    module: &HirModule,
) -> MirLoweringResult {
    let main_def = typecheck.main_fn_def;

    let hir_fns_by_def = crate::collecter::collect_hir_fns(module);
    let mut lowering = MirLowering::new(rodeo, typecheck, resolution, module, &hir_fns_by_def);

    let mut main_fn: Option<MirFunctionId> = None;

    if let Some(main_def) = main_def {
        let main_fn_monomorphized = lowering.monomorphize_fn(main_def, Vec::new());
        lowering.set_function_name(main_fn_monomorphized, "main");

        main_fn = Some(main_fn_monomorphized);
    } else {
        hir_fns_by_def.keys().for_each(|&def_id| {
            lowering.monomorphize_fn(def_id, Vec::new());
        });
    }

    MirLoweringResult {
        program: lowering.finish(),
        main_fn,
    }
}

pub struct MirLowering<'ctx> {
    rodeo: Rc<RefCell<Rodeo>>,

    typecheck: &'ctx mut TypeCheckResult,
    resolution: &'ctx ResolutionResult,
    hir_fns_by_def: &'ctx HashMap<DefId, Rc<HirFn>>,

    program: MirProgram,
    mono_cache: MonoCache,
}

#[derive(Default)]
pub struct MonoCache {
    cache: HashMap<(DefId, Vec<TypeId>), MirFunctionId>,
    next_id: u32,
}

impl MonoCache {
    pub fn new() -> Self {
        Self::default()
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
    pub fn new(source_def: DefId, mono_args: Vec<TypeId>, entry: BlockId, ret_ty: TypeId) -> Self {
        Self {
            func: MirFunction {
                source_def,
                mono_args,
                locals: Vec::new(),
                blocks: Vec::new(),
                params: Vec::new(),
                entry_block: entry,
                ret_ty,
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
        rodeo: Rc<RefCell<Rodeo>>,
        typecheck: &'ctx mut TypeCheckResult,
        resolution: &'ctx ResolutionResult,
        module: &HirModule,
        hir_fns_by_def: &'ctx HashMap<DefId, Rc<HirFn>>,
    ) -> Self {
        Self {
            rodeo,
            typecheck,
            resolution,
            program: MirProgram::default(),
            mono_cache: MonoCache::new(),
            hir_fns_by_def,
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

    fn set_function_name(&mut self, id: MirFunctionId, name: impl AsRef<str>) {
        self.program.function_names.insert(id, name.as_ref().into());
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

    fn display_type_name(&self, ty: TypeId) -> String {
        self.typecheck.interner.get(ty).to_display(
            Rc::clone(&self.rodeo),
            &self.typecheck.interner,
            self.resolution,
        )
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
                if let Some(op_res) = self.typecheck.operator_resolutions.get(&expr.id).cloned() {
                    let (block, rhs_op) = self.lower_expr_to_operand(fb, rhs, block);
                    let result_ty = self.expr_type(expr);
                    return self.lower_operator_method_call_with_extra_args(
                        fb,
                        lhs,
                        &[rhs_op],
                        &op_res,
                        block,
                        result_ty,
                    );
                }

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

            HirExprKind::Unary {
                expr: inner,
                op: UnaryOp::AddrOf,
            } => {
                let (block, inner_place) = self.lower_expr_to_place(fb, inner, block);
                let result_ty = self.expr_type(expr);

                let is_const = match self.typecheck.interner.get(result_ty).clone() {
                    Type::Pointer { is_const, .. } => is_const,
                    _ => false,
                };

                let temp = fb.new_temp(result_ty);
                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place: Place::from_local(temp),
                        rvalue: Rvalue::Ref {
                            place: inner_place,
                            is_const,
                        },
                    },
                );

                (block, Operand::Move(Place::from_local(temp)))
            }

            HirExprKind::Unary { expr: inner, op } => {
                if let Some(op_res) = self.typecheck.operator_resolutions.get(&expr.id).cloned() {
                    return self.lower_operator_method_call(
                        fb,
                        inner,
                        &op_res,
                        block,
                        self.expr_type(expr),
                    );
                }

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

            HirExprKind::SliceAccess { object, index } => {
                if let Some(op_res) = self.typecheck.operator_resolutions.get(&expr.id).cloned() {
                    let (block, index_operand) = self.lower_expr_to_operand(fb, index, block);
                    let result_ty = self.expr_type(expr);

                    return self.lower_operator_method_call_with_extra_args(
                        fb,
                        object,
                        &[index_operand],
                        &op_res,
                        block,
                        result_ty,
                    );
                }

                let obj_ty = self.expr_type(object);
                let (block, obj_place) = self.lower_expr_to_place(fb, object, block);
                let (block, index_operand) = self.lower_expr_to_operand(fb, index, block);

                let index_local =
                    self.operand_to_local(fb, index_operand, self.expr_type(index), block);

                let elem_place = match self.typecheck.interner.get(obj_ty).clone() {
                    Type::Array { .. } | Type::ManyPointer { .. } => obj_place.index(index_local),
                    Type::Slice { .. } => {
                        let mut ptr_place = obj_place;
                        ptr_place.projection.push(PlaceElem::SlicePtr);
                        ptr_place.index(index_local)
                    }
                    _ => unreachable!(),
                };

                let ty = self.expr_type(expr);

                (block, self.place_to_operand(elem_place, ty))
            }

            HirExprKind::FieldAccess { .. } => {
                let (block, place) = self.lower_expr_to_place(fb, expr, block);
                let ty = self.expr_type(expr);
                (block, self.place_to_operand(place, ty))
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
                        discriminant: cond_operand,
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
                    unreachable!("must been recorded this table");
                };

                let call_target = self.resolve_call_target(fn_def, generic_args, &hir_fn);

                let mut arg_operands = Vec::with_capacity(args.len() + 1);

                if let HirExprKind::FieldAccess { object, .. } = &callee.kind {
                    let (b, self_operand) = self.lower_receiver_operand(fb, object, fn_def, block);
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
                        func: call_target,
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

            HirExprKind::MacroCall { kind, args } => match kind.0 {
                HirMacroKind::SizeOf | HirMacroKind::AlignOf => {
                    let target_ty = match &args[0].kind {
                        HirExprKind::Type(_) => self.expr_type(&args[0]),
                        _ => panic!("@sizeof / @alignof arg must be a type expression"),
                    };

                    let result_ty = self.expr_type(expr);
                    let temp = fb.new_temp(result_ty);
                    let rvalue = if matches!(kind.0, HirMacroKind::SizeOf) {
                        Rvalue::SizeOf(target_ty)
                    } else {
                        Rvalue::AlignOf(target_ty)
                    };

                    fb.push_stmt(
                        block,
                        MirStatement::Assign {
                            place: Place::from_local(temp),
                            rvalue,
                        },
                    );
                    (block, Operand::Move(Place::from_local(temp)))
                }

                HirMacroKind::As => {
                    let target_ty = match &args[0].kind {
                        HirExprKind::Type(_) => self.expr_type(&args[0]),
                        _ => panic!("@as first arg must be a type expression"),
                    };

                    let (block, value_operand) = self.lower_expr_to_operand(fb, &args[1], block);

                    let temp = fb.new_temp(target_ty);
                    fb.push_stmt(
                        block,
                        MirStatement::Assign {
                            place: Place::from_local(temp),
                            rvalue: Rvalue::Cast {
                                operand: value_operand,
                                target: target_ty,
                            },
                        },
                    );
                    (block, Operand::Move(Place::from_local(temp)))
                }

                HirMacroKind::Print
                | HirMacroKind::Println
                | HirMacroKind::Format
                | HirMacroKind::Dbg
                | HirMacroKind::Panic => self.lower_macro_call(fb, kind.0, args, expr.id, block),

                HirMacroKind::Unreachable | HirMacroKind::Todo => {
                    self.lower_diverging_macro(fb, kind.0, block)
                }

                HirMacroKind::Unknown => panic!("unknown macro reached MIR lowering"),
            },

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

            HirExprKind::Switch => unreachable!("not implemented in previous stages"),
            HirExprKind::GenericParamRef(_) => unreachable!(),
            HirExprKind::Type(_) => unreachable!(),
            HirExprKind::Error => unreachable!(),
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

            _ => panic!("passed `expr-to-place` is not lvalue"),
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
        method_def_id: DefId,
        block: BlockId,
    ) -> (BlockId, Operand) {
        let method_ty = self
            .typecheck
            .def_types
            .get(&method_def_id)
            .copied()
            .expect("method must have a recorded Fn type");

        let expected_self_ty = match self.typecheck.interner.get(method_ty).clone() {
            Type::Fn { params, .. } if !params.is_empty() => Some(params[0]),
            _ => None,
        };

        let obj_ty = self.expr_type(object);
        let (block, place) = self.lower_expr_to_place(fb, object, block);

        match expected_self_ty.map(|t| self.typecheck.interner.get(t).clone()) {
            Some(Type::Struct { .. }) | None => (block, self.place_to_operand(place, obj_ty)),

            Some(Type::Pointer { is_const, .. }) => {
                match self.typecheck.interner.get(obj_ty).clone() {
                    Type::Pointer { .. } => (block, self.place_to_operand(place, obj_ty)),
                    _ => {
                        let ptr_ty = self.typecheck.interner.intern(Type::Pointer {
                            inner: obj_ty,
                            is_const,
                        });

                        let temp = fb.new_temp(ptr_ty);

                        fb.push_stmt(
                            block,
                            MirStatement::Assign {
                                place: Place::from_local(temp),
                                rvalue: Rvalue::Ref { place, is_const },
                            },
                        );
                        (block, Operand::Move(Place::from_local(temp)))
                    }
                }
            }

            _ => (block, self.place_to_operand(place, obj_ty)),
        }
    }

    fn operand_to_local(
        &mut self,
        fb: &mut FnBuilder,
        operand: Operand,
        ty: TypeId,
        block: BlockId,
    ) -> LocalId {
        match &operand {
            Operand::Copy(place) | Operand::Move(place) if place.projection.is_empty() => {
                place.local
            }
            _ => {
                let temp = fb.new_temp(ty);
                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place: Place::from_local(temp),
                        rvalue: Rvalue::Use(operand),
                    },
                );
                temp
            }
        }
    }

    fn lower_macro_call(
        &mut self,
        fb: &mut FnBuilder,
        kind: HirMacroKind,
        args: &[Rc<HirExpr>],
        hir_id: HirId,
        block: BlockId,
    ) -> (BlockId, Operand) {
        let format_chunks = self.typecheck.format_specs.get(&hir_id).cloned();

        let value_exprs: &[Rc<HirExpr>] = if format_chunks.is_some() {
            &args[1..]
        } else {
            args
        };

        let mut block = block;
        let mut operands = Vec::with_capacity(value_exprs.len());
        for arg in value_exprs {
            let (b, op) = self.lower_expr_to_operand(fb, arg, block);
            block = b;
            operands.push(op);
        }

        let result_ty = self
            .typecheck
            .expr_types
            .get(&hir_id)
            .copied()
            .unwrap_or_else(|| self.typecheck.interner.intern(Type::Void));

        let dest = fb.new_temp(result_ty);
        let next = fb.new_block();

        let is_diverging = matches!(kind, HirMacroKind::Panic);

        fb.set_terminator(
            block,
            Terminator::MacroCall {
                kind,
                format_chunks,
                args: operands,
                destination: Place::from_local(dest),
                target: if is_diverging { None } else { Some(next) },
            },
        );

        if is_diverging {
            fb.set_terminator(next, Terminator::Unreachable);
            (next, Operand::Constant(ConstValue::Void))
        } else {
            (
                next,
                self.place_to_operand(Place::from_local(dest), result_ty),
            )
        }
    }

    fn lower_diverging_macro(
        &mut self,
        fb: &mut FnBuilder,
        kind: HirMacroKind,
        block: BlockId,
    ) -> (BlockId, Operand) {
        let void_ty = self.typecheck.interner.intern(Type::Void);
        let dest = fb.new_temp(void_ty);
        let next = fb.new_block();

        fb.set_terminator(
            block,
            Terminator::MacroCall {
                kind,
                format_chunks: None,
                args: Vec::new(),
                destination: Place::from_local(dest),
                target: None,
            },
        );

        fb.set_terminator(next, Terminator::Unreachable);
        (next, Operand::Constant(ConstValue::Void))
    }

    fn lower_operator_method_call(
        &mut self,
        fb: &mut FnBuilder,
        reciever_expr: &HirExpr,
        op_res: &OperatorResolution,
        block: BlockId,
        result_ty: TypeId,
    ) -> (BlockId, Operand) {
        self.lower_operator_method_call_with_extra_args(
            fb,
            reciever_expr,
            &[],
            op_res,
            block,
            result_ty,
        )
    }

    fn lower_operator_method_call_with_extra_args(
        &mut self,
        fb: &mut FnBuilder,
        reciever_expr: &HirExpr,
        extra_args: &[Operand],
        op_res: &OperatorResolution,
        block: BlockId,
        result_ty: TypeId,
    ) -> (BlockId, Operand) {
        let Some(hir_fn) = self.hir_fns_by_def.get(&op_res.method_def).cloned() else {
            panic!("operator method {:?} has no HIR body", op_res.method_def);
        };

        let mir_fn_id = self.monomorphize_fn(op_res.method_def, op_res.generic_args.clone());

        let (block, self_operand) =
            self.lower_receiver_operand(fb, reciever_expr, op_res.method_def, block);

        let mut args = vec![self_operand];
        args.extend_from_slice(extra_args);

        let dest = fb.new_temp(result_ty);
        let next = fb.new_block();

        fb.set_terminator(
            block,
            Terminator::Call {
                func: CallTarget::Direct(mir_fn_id),
                args,
                destination: Place::from_local(dest),
                target: Some(next),
            },
        );

        (
            next,
            self.place_to_operand(Place::from_local(dest), result_ty),
        )
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
                        discriminant: cond_operand,
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

            HirStmtKind::For {
                def_id,
                iterator,
                block: body,
                ..
            } => {
                let (block, iter_ty) = {
                    let ty = self.expr_type(iterator);
                    (block, ty)
                };

                match self.typecheck.interner.get(iter_ty).clone() {
                    Type::Builtin(b) if builtin_is_integer(b) => {
                        self.lower_for_range(fb, def_id, iterator, body, block)
                    }

                    Type::Array { .. } | Type::Slice { .. } => {
                        self.lower_for_iterable(fb, def_id, iterator, iter_ty, body, block)
                    }

                    _ => panic!("non-iterable type passed Typechecker: {:?}", iter_ty),
                }
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

            HirStmtKind::Error => panic!("Error Statement kind passed in MIR lowering stage"),
        }
    }

    fn lower_for_range(
        &mut self,
        fb: &mut FnBuilder,
        def_id: &DefId,
        iterator: &HirExpr,
        body: &HirStmt,
        block: BlockId,
    ) -> BlockId {
        let (block, count_operand) = self.lower_expr_to_operand(fb, iterator, block);

        let usize_ty = self
            .typecheck
            .interner
            .intern(Type::Builtin(zeen_ast::types::BuiltinType::usize));
        let counter = fb.new_local(usize_ty, LocalKind::Temporary, Mutability::Mut, None, None);
        fb.push_stmt(
            block,
            MirStatement::Assign {
                place: Place::from_local(counter),
                rvalue: Rvalue::Use(Operand::Constant(ConstValue::Int(0))),
            },
        );

        let header = fb.new_block();
        fb.set_terminator(block, Terminator::Goto(header));

        let loop_var_ty = self
            .typecheck
            .def_types
            .get(def_id)
            .copied()
            .unwrap_or(usize_ty);
        let loop_var = fb.new_local(
            loop_var_ty,
            LocalKind::UserVariable,
            Mutability::Const,
            None,
            None,
        );
        fb.locals_by_def.insert(*def_id, loop_var);

        fb.push_stmt(
            header,
            MirStatement::Assign {
                place: Place::from_local(loop_var),
                rvalue: Rvalue::Use(Operand::Copy(Place::from_local(counter))),
            },
        );

        let bool_ty = self
            .typecheck
            .interner
            .intern(Type::Builtin(zeen_ast::types::BuiltinType::bool));
        let cmp_result = fb.new_temp(bool_ty);
        fb.push_stmt(
            header,
            MirStatement::Assign {
                place: Place::from_local(cmp_result),
                rvalue: Rvalue::BinaryOp {
                    op: BinaryOp::Lt,
                    lhs: Operand::Copy(Place::from_local(counter)),
                    rhs: count_operand,
                },
            },
        );

        let body_bb = fb.new_block();
        let exit_bb = fb.new_block();

        fb.set_terminator(
            header,
            Terminator::SwitchInt {
                discriminant: Operand::Move(Place::from_local(cmp_result)),
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

        let incremented = fb.new_temp(usize_ty);
        fb.push_stmt(
            body_end,
            MirStatement::Assign {
                place: Place::from_local(incremented),
                rvalue: Rvalue::BinaryOp {
                    op: BinaryOp::Add,
                    lhs: Operand::Copy(Place::from_local(counter)),
                    rhs: Operand::Constant(ConstValue::Int(1)),
                },
            },
        );
        fb.push_stmt(
            body_end,
            MirStatement::Assign {
                place: Place::from_local(counter),
                rvalue: Rvalue::Use(Operand::Move(Place::from_local(incremented))),
            },
        );
        fb.set_terminator(body_end, Terminator::Goto(header));

        exit_bb
    }

    fn lower_for_iterable(
        &mut self,
        fb: &mut FnBuilder,
        def_id: &DefId,
        iterator: &HirExpr,
        iter_ty: TypeId,
        body: &HirStmt,
        block: BlockId,
    ) -> BlockId {
        let (block, iter_place) = self.lower_expr_to_place(fb, iterator, block);

        let usize_ty = self
            .typecheck
            .interner
            .intern(Type::Builtin(zeen_ast::types::BuiltinType::usize));

        let (len_operand, elem_ty) = match self.typecheck.interner.get(iter_ty).clone() {
            Type::Array { element, len } => {
                let len_val = len.expect("unknown array length (must be comptime known)");

                (Operand::Constant(ConstValue::Int(len_val as i128)), element)
            }
            Type::Slice { element, .. } => {
                let mut len_place = iter_place.clone();
                len_place.projection.push(PlaceElem::SliceLen);

                let len_local = fb.new_temp(usize_ty);

                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place: Place::from_local(len_local),
                        rvalue: Rvalue::Use(Operand::Copy(len_place)),
                    },
                );

                (Operand::Move(Place::from_local(len_local)), element)
            }

            _err_type => panic!("non-iterable type: {:?}", _err_type),
        };

        let counter = fb.new_local(usize_ty, LocalKind::Temporary, Mutability::Mut, None, None);
        fb.push_stmt(
            block,
            MirStatement::Assign {
                place: Place::from_local(counter),
                rvalue: Rvalue::Use(Operand::Constant(ConstValue::Int(0))),
            },
        );

        let header = fb.new_block();
        fb.set_terminator(block, Terminator::Goto(header));

        let bool_ty = self
            .typecheck
            .interner
            .intern(Type::Builtin(zeen_ast::types::BuiltinType::bool));

        let cmp_result = fb.new_temp(bool_ty);

        fb.push_stmt(
            header,
            MirStatement::Assign {
                place: Place::from_local(cmp_result),
                rvalue: Rvalue::BinaryOp {
                    op: BinaryOp::Lt,
                    lhs: Operand::Copy(Place::from_local(counter)),
                    rhs: len_operand,
                },
            },
        );

        let body_bb = fb.new_block();
        let exit_bb = fb.new_block();

        fb.set_terminator(
            header,
            Terminator::SwitchInt {
                discriminant: Operand::Move(Place::from_local(cmp_result)),
                targets: vec![(1, body_bb)],
                otherwise: exit_bb,
            },
        );

        let loop_var = fb.new_local(
            elem_ty,
            LocalKind::UserVariable,
            Mutability::Const,
            None,
            None,
        );

        fb.locals_by_def.insert(*def_id, loop_var);

        let elem_place = match self.typecheck.interner.get(iter_ty).clone() {
            Type::Array { .. } => iter_place.clone().index(counter),
            Type::Slice { .. } => {
                let mut ptr_place = iter_place.clone();

                ptr_place.projection.push(PlaceElem::SlicePtr);
                ptr_place.index(counter)
            }
            _ => unreachable!(),
        };

        let elem_operand = self.place_to_operand(elem_place, elem_ty);

        fb.push_stmt(
            body_bb,
            MirStatement::Assign {
                place: Place::from_local(loop_var),
                rvalue: Rvalue::Use(elem_operand),
            },
        );

        fb.loop_stack.push(LoopTargets {
            break_target: exit_bb,
            continue_target: header,
        });

        let body_end = self.lower_stmt_as_block_value(fb, body, body_bb).0;

        fb.loop_stack.pop();

        let incremented = fb.new_temp(usize_ty);

        fb.push_stmt(
            body_end,
            MirStatement::Assign {
                place: Place::from_local(incremented),
                rvalue: Rvalue::BinaryOp {
                    op: BinaryOp::Add,
                    lhs: Operand::Copy(Place::from_local(counter)),
                    rhs: Operand::Constant(ConstValue::Int(1)),
                },
            },
        );

        fb.push_stmt(
            body_end,
            MirStatement::Assign {
                place: Place::from_local(counter),
                rvalue: Rvalue::Use(Operand::Move(Place::from_local(incremented))),
            },
        );

        fb.set_terminator(body_end, Terminator::Goto(header));

        exit_bb
    }
}

impl<'ctx> MirLowering<'ctx> {
    fn monomorphize_fn(&mut self, def_id: DefId, generic_args: Vec<TypeId>) -> MirFunctionId {
        let hir_fn = self.hir_fns_by_def[&def_id].clone();
        let key = (def_id, generic_args.clone());

        if let Some(&existing) = self.mono_cache.cache.get(&key) {
            return existing;
        }

        let id = self.mono_cache.fresh_id();
        self.mono_cache.cache.insert(key, id);

        let mir_func = self.lower_fn_body(def_id, &hir_fn, &generic_args);
        self.program.functions.insert(id, mir_func);

        let interner = self.rodeo.borrow();
        let base_name = interner.resolve(&hir_fn.name.0).to_string();
        drop(interner);

        if hir_fn.is_extern {
            self.set_function_name(id, base_name.clone());
            self.program.extern_exports.insert(id, base_name);
        } else {
            let display_name = if generic_args.is_empty() {
                base_name
            } else {
                let arg_names: Vec<String> = generic_args
                    .iter()
                    .map(|&t| self.display_type_name(t))
                    .collect();
                format!("{}${}", base_name, arg_names.join("_"))
            };

            self.set_function_name(id, display_name);
        }

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

        let fn_ty = self
            .typecheck
            .def_types
            .get(&def_id)
            .copied()
            .expect("function must have recorded fn type");

        let raw_ret_ty = match self.typecheck.interner.get(fn_ty) {
            Type::Fn { ret, .. } => *ret,
            _ => unreachable!(),
        };

        let ret_ty =
            zeen_types::substitute_generics(&mut self.typecheck.interner, raw_ret_ty, &bindings);

        let entry = BlockId(0);
        let mut fb = FnBuilder::new(def_id, generic_args.to_vec(), entry, ret_ty);
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
        ret_ty: TypeId,
    ) -> (BlockId, Operand) {
        let (mut block, callee_operand) = self.lower_expr_to_operand(fb, callee, block);

        let mut arg_operands = Vec::with_capacity(args.len());
        for arg in args.iter() {
            let (b, op) = self.lower_expr_to_operand(fb, arg, block);
            block = b;
            arg_operands.push(op);
        }

        let dest_local = fb.new_temp(ret_ty);
        let dest_place = Place::from_local(dest_local);
        let next_block = fb.new_block();

        let is_diverging = matches!(self.typecheck.interner.get(ret_ty), Type::Never);

        fb.set_terminator(
            block,
            Terminator::Call {
                func: CallTarget::Indirect(callee_operand),
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

    fn resolve_call_target(
        &mut self,
        fn_def: DefId,
        generic_args: Vec<TypeId>,
        hir_fn: &Rc<HirFn>,
    ) -> CallTarget {
        if hir_fn.is_extern && hir_fn.body.is_none() {
            let idx = self.register_extern_fn(fn_def, hir_fn);
            CallTarget::Extern(idx)
        } else {
            let mir_id = self.monomorphize_fn(fn_def, generic_args);
            CallTarget::Direct(mir_id)
        }
    }

    fn register_extern_fn(&mut self, fn_def: DefId, hir_fn: &HirFn) -> usize {
        let symbol_name = self.rodeo.borrow().resolve(&hir_fn.name.0).to_string();

        if let Some(idx) = self
            .program
            .extern_fns
            .iter()
            .position(|f| f.symbol_name == symbol_name)
        {
            return idx;
        }

        let fn_ty = self
            .typecheck
            .def_types
            .get(&fn_def)
            .copied()
            .expect("no recorded fn type found");

        let (param_types, ret_ty) = match self.typecheck.interner.get(fn_ty).clone() {
            Type::Fn { params, ret } => (params, ret),
            _ => panic!("recorded extern fn type is not `Fn`"),
        };

        let is_variadic = hir_fn
            .params
            .last()
            .map(|p| matches!(p.ty.kind, zeen_hir::types::HirTypeKind::VaArgs))
            .unwrap_or(false);

        let param_types = if is_variadic {
            param_types[..param_types.len().saturating_sub(1)].to_vec()
        } else {
            param_types
        };

        self.program.extern_fns.push(ExternFnDecl {
            symbol_name,
            param_types,
            ret_ty,
            is_variadic,
        });

        self.program.extern_fns.len() - 1
    }
}
