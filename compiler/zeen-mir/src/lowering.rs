use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use lasso::{Rodeo, Spur};
use smol_str::SmolStr;
use zeen_ast::{
    Source,
    expressions::{BinaryOp, Literal, UnaryOp},
};
use zeen_driver::CompilationMode;
use zeen_hir::{
    HirId, HirMacroKind, HirModule, HirTypeExpr,
    decl::{HirDecl, HirDeclKind, HirFn},
    expr::{HirExpr, HirExprKind},
    stmt::{HirStmt, HirStmtKind},
};
use zeen_resolve::{DefId, DefKind, ResolutionResult};
use zeen_typecheck::{
    coerce::builtin_is_integer,
    format_str::{FormatChunk, FormatSpec, arg_specs},
    result::{CallResolution, OperatorResolution, TypeCheckResult},
};
use zeen_types::{
    CLOSURE_FAT_DEF, CLOSURE_FAT_ENV_FIELD, CLOSURE_FAT_FN_FIELD, FatFnBody, SLICE_LEN_FIELD,
    SLICE_PTR_FIELD, SLICE_STRUCT_DEF, StructTypeInfo, Type, TypeId, TypeInterner,
    closure_field_def, closure_struct_def,
};

use crate::error::{MirError, MirWarning};
use crate::{
    AggregateKind, BasicBlock, BlockId, CallTarget, ConstValue, ExternFnDecl, LocalDecl, LocalId,
    LocalKind, MirFunction, MirFunctionId, MirGlobalVar, MirGlobalVarId, MirProgram, MirStatement,
    Mutability, Operand, Place, PlaceElem, Rvalue, StructFieldLayout, StructLayout, Terminator,
};

pub struct MirLoweringResult {
    pub program: MirProgram,
    pub main_fn: Option<MirFunctionId>,
    pub warnings: Vec<MirWarning>,
}

pub fn lower_program<'ctx>(
    rodeo: Rc<RefCell<Rodeo>>,
    typecheck: &'ctx mut TypeCheckResult,
    resolution: &'ctx ResolutionResult,
    module: &HirModule,
    mode: CompilationMode,
) -> Result<MirLoweringResult, Vec<MirError>> {
    let main_def = typecheck.main_fn_def;

    let hir_fns_by_def = crate::collecter::collect_hir_fns(module);
    // Only top-level (non-nested) functions are eagerly lowered; nested ones
    // are registered when the enclosing function actually calls them.
    let fns_with_owners: Vec<(DefId, Rc<HirFn>, Option<DefId>)> = hir_fns_by_def
        .iter()
        .filter(|(_, f)| f.parent_fn.is_none())
        .map(|f| {
            (
                *f.0,
                Rc::clone(f.1),
                typecheck.method_owner.get(f.0).copied(),
            )
        })
        .collect();

    let mut lowering = MirLowering::new(
        Rc::clone(&rodeo),
        typecheck,
        resolution,
        module,
        &hir_fns_by_def,
        mode,
    );

    lowering.register_globals();

    let mut main_fn: Option<MirFunctionId> = None;

    if let Some(main_def) = main_def {
        let main_fn_monomorphized = lowering.monomorphize_fn(main_def, Vec::new(), None, &[]);
        lowering.set_function_name(main_fn_monomorphized, "main");

        main_fn = Some(main_fn_monomorphized);
    } else {
        fns_with_owners.iter().for_each(|(def_id, _, owner)| {
            let is_core = resolution
                .defs
                .get(def_id)
                .map(|info| info.span.src().name().starts_with("core."))
                .unwrap_or(false);

            if !is_core {
                lowering.monomorphize_fn(*def_id, Vec::new(), *owner, &[]);
            }
        });

        lowering.register_user_struct_layouts(resolution);
    }

    lowering.build_init_globals();
    lowering.register_drop_functions();
    lowering.register_reachable_fat_layouts();
    lowering.register_fat_drop_functions();

    let warnings = std::mem::take(&mut lowering.warnings);
    let mut program = lowering.finish()?;
    let extern_vars = crate::collecter::collect_extern_vars(module, typecheck, &rodeo);

    program.extern_vars = extern_vars;

    Ok(MirLoweringResult {
        program,
        main_fn,
        warnings,
    })
}

pub struct MirLowering<'ctx> {
    rodeo: Rc<RefCell<Rodeo>>,

    typecheck: &'ctx mut TypeCheckResult,
    resolution: &'ctx ResolutionResult,
    module: &'ctx HirModule,
    hir_fns_by_def: &'ctx HashMap<DefId, Rc<HirFn>>,

    program: MirProgram,
    mono_cache: MonoCache,

    errors: Vec<MirError>,
    warnings: Vec<MirWarning>,

    /// Stack of functions currently being lowered. Used to resolve the parent
    /// of a nested function so its MIR name becomes `<parent>-><name>`.
    fn_stack: Vec<FnContext>,

    /// Cache of synthesized bare-fn → fat-`{fn,env}` adapter functions, keyed
    /// by (target, fat signature) so each adapter is emitted once.
    fat_adapter_cache: HashMap<(MirFunctionId, TypeId), MirFunctionId>,

    /// Cache of synthesized per-fat-type drop functions (env teardown +
    /// `free`), keyed by the fat `TypeId`.
    fat_drop_cache: HashMap<TypeId, MirFunctionId>,

    /// Fat types of closures whose value is never used: no env is
    /// materialized for them (`null`), so they must not get a drop function.
    unused_fat_types: HashSet<TypeId>,

    mode: CompilationMode,

    globals: Vec<GlobalDecl>,
    globals_by_def: HashMap<DefId, MirGlobalVarId>,
}

struct GlobalDecl {
    def_id: DefId,
    name: Spur,
    value: Rc<HirExpr>,
    is_const: bool,
    is_pub: bool,
}

/// The enclosing function of the one being lowered, used to prefix nested
/// function names with their parent.
#[derive(Debug, Clone)]
pub struct FnContext {
    pub def_id: DefId,
    pub readable_name: String,
}

#[derive(Default)]
pub struct MonoCache {
    /// Generic instantiations plus per-call-site fat (closure) parameter
    /// bindings: the third key component is the concrete fat type bound to
    /// each `Fn`/`FnOnce`-typed parameter, in parameter order.
    cache: HashMap<(DefId, Vec<TypeId>, Vec<TypeId>), MirFunctionId>,
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
    /// For closure bodies: captured variables referenced through the env
    /// pointer. Maps the captured `DefId` to `env.deref().field(..)`. Looked up
    /// after `locals_by_def` so ordinary locals still win.
    captured_places: HashMap<DefId, Place>,
    loop_stack: Vec<LoopTargets>,
    bindings: HashMap<DefId, TypeId>,
    /// Concrete fat types bound to this function's `Fn`/`FnOnce` parameters
    /// (param `DefId` → concrete fat type), filled in per monomorphized copy.
    fat_bindings: Vec<(DefId, TypeId)>,
    /// Lexical scope stack. Each active `HirExprKind::Block` pushes an entry;
    /// every `let` inside it registers its local. On scope exit the locals are
    /// emitted as `StorageDead`, giving zeen-flow a per-scope drop point.
    scope_stack: Vec<Vec<LocalId>>,
}

impl FnBuilder {
    pub fn new(
        source_def: DefId,
        mono_args: Vec<TypeId>,
        entry: BlockId,
        ret_ty: TypeId,
        bindings: HashMap<DefId, TypeId>,
    ) -> Self {
        Self {
            func: MirFunction {
                source_def,
                mono_args,
                locals: Vec::new(),
                blocks: Vec::new(),
                params: Vec::new(),
                entry_block: entry,
                ret_ty,
                is_drop_impl: false,
            },
            locals_by_def: HashMap::new(),
            captured_places: HashMap::new(),
            loop_stack: Vec::new(),
            bindings,
            fat_bindings: Vec::new(),
            scope_stack: Vec::new(),
        }
    }

    /// The place backing a `DefId` inside this frame: an ordinary local first,
    /// then a captured variable exposed through the closure env pointer.
    fn place_for_def(&self, def_id: DefId) -> Option<Place> {
        if let Some(local) = self.locals_by_def.get(&def_id) {
            Some(Place::from_local(*local))
        } else {
            self.captured_places.get(&def_id).cloned()
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

    /// Appends a fallthrough `Goto` only when the block does not already end
    /// with a terminator (e.g. `break`/`continue`/`return` inside an `if`
    /// branch). Overwriting those would silently swallow the early exit.
    fn join_if_open(&mut self, block: BlockId, join: BlockId) {
        if self.block_is_open(block) {
            self.set_terminator(block, Terminator::Goto(join));
        }
    }

    fn block_is_open(&self, block: BlockId) -> bool {
        matches!(
            self.func.block(block).terminator,
            Terminator::Unreachable // placeholder for a block that still needs a terminator
        )
    }
}

impl<'ctx> MirLowering<'ctx> {
    pub fn new(
        rodeo: Rc<RefCell<Rodeo>>,
        typecheck: &'ctx mut TypeCheckResult,
        resolution: &'ctx ResolutionResult,
        module: &'ctx HirModule,
        hir_fns_by_def: &'ctx HashMap<DefId, Rc<HirFn>>,
        mode: CompilationMode,
    ) -> Self {
        Self {
            rodeo,
            typecheck,
            resolution,
            module,
            program: MirProgram::default(),
            mono_cache: MonoCache::new(),
            hir_fns_by_def,
            fn_stack: Vec::new(),
            fat_adapter_cache: HashMap::new(),
            fat_drop_cache: HashMap::new(),
            unused_fat_types: HashSet::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            mode,
            globals: Vec::new(),
            globals_by_def: HashMap::new(),
        }
    }

    pub fn finish(mut self) -> Result<MirProgram, Vec<MirError>> {
        if !self.errors.is_empty() {
            return Err(self.errors);
        }

        self.register_reachable_slice_layouts();
        Ok(self.program)
    }

    fn register_globals(&mut self) {
        for decl in &self.module.decls {
            let HirDeclKind::GlobalVar {
                name,
                value,
                is_const,
                is_pub,
                ..
            } = &decl.kind
            else {
                continue;
            };

            let ty = self
                .typecheck
                .def_types
                .get(&decl.def_id)
                .copied()
                .unwrap_or_else(|| self.typecheck.interner.intern(Type::Error));
            let symbol_name = self.rodeo.borrow().resolve(&name.0).to_string();

            let id = MirGlobalVarId(self.program.global_vars.len() as u32);
            self.globals_by_def.insert(decl.def_id, id);
            self.globals.push(GlobalDecl {
                def_id: decl.def_id,
                name: name.0,
                value: Rc::clone(value),
                is_const: *is_const,
                is_pub: *is_pub,
            });
            self.program.global_vars.push(MirGlobalVar {
                def_id: decl.def_id,
                symbol_name,
                ty,
                is_const: *is_const,
                is_pub: *is_pub,
            });
        }
    }

    fn build_init_globals(&mut self) {
        if self.globals.is_empty() {
            return;
        }

        let ordered = self.globals_in_init_order();

        let void_ty = self.typecheck.interner.intern(Type::Void);
        let entry = BlockId(0);
        let mut fb = FnBuilder::new(DefId(u32::MAX), Vec::new(), entry, void_ty, HashMap::new());
        fb.new_block();

        for def_id in ordered {
            let id = self.globals_by_def[&def_id];
            let value = Rc::clone(
                &self
                    .globals
                    .iter()
                    .find(|g| g.def_id == def_id)
                    .unwrap()
                    .value,
            );
            let (block, operand) = self.lower_expr_to_operand(&mut fb, &value, entry);

            if let Operand::Copy(place, _) | Operand::Move(place, _) = &operand
                && matches!(
                    self.typecheck.interner.get(fb.func.local(place.local).ty),
                    Type::Void
                )
            {
                continue;
            }

            fb.push_stmt(
                block,
                MirStatement::Assign {
                    place: Place::global(id),
                    rvalue: Rvalue::Use(operand),
                    source: None,
                },
            );

            fb.set_terminator(block, Terminator::Goto(entry));
        }

        fb.set_terminator(
            entry,
            Terminator::Return(Operand::Constant(ConstValue::Void, None)),
        );

        let id = self.mono_cache.fresh_id();
        self.set_function_name(id, "zeen_init_globals");
        self.program.functions.insert(id, fb.func);
        self.program.init_globals_fn = Some(id);
    }

    fn globals_in_init_order(&self) -> Vec<DefId> {
        let mut order = Vec::with_capacity(self.globals.len());
        let mut visited = HashSet::new();
        for global in &self.globals {
            self.visit_global_init(global.def_id, &mut visited, &mut order);
        }
        order
    }

    fn visit_global_init(
        &self,
        def_id: DefId,
        visited: &mut HashSet<DefId>,
        order: &mut Vec<DefId>,
    ) {
        if !visited.insert(def_id) {
            return;
        }
        if let Some(global) = self.globals.iter().find(|g| g.def_id == def_id) {
            let mut deps: Vec<DefId> = Vec::new();
            self.collect_global_expr_deps(&global.value, &mut deps);
            for dep in deps {
                self.visit_global_init(dep, visited, order);
            }
        }
        order.push(def_id);
    }

    fn collect_global_expr_deps(&self, expr: &HirExpr, out: &mut Vec<DefId>) {
        match &expr.kind {
            HirExprKind::VarRef(def_id) | HirExprKind::SelfValue(def_id) => {
                if matches!(
                    self.resolution.defs.get(def_id).map(|info| &info.kind),
                    Some(DefKind::GlobalVar { .. })
                ) {
                    out.push(*def_id);
                }
            }
            HirExprKind::Binary { lhs, rhs, .. } => {
                self.collect_global_expr_deps(lhs, out);
                self.collect_global_expr_deps(rhs, out);
            }
            HirExprKind::Unary { expr: inner, .. } => {
                self.collect_global_expr_deps(inner, out);
            }
            HirExprKind::Call { callee, args, .. } => {
                self.collect_global_expr_deps(callee, out);
                for arg in args {
                    self.collect_global_expr_deps(arg, out);
                }
            }
            HirExprKind::MacroCall { args, .. } => {
                for arg in args {
                    self.collect_global_expr_deps(arg, out);
                }
            }
            HirExprKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.collect_global_expr_deps(condition, out);
                self.collect_global_stmt_deps(then_block, out);
                if let Some(else_block) = else_block {
                    self.collect_global_stmt_deps(else_block, out);
                }
            }
            HirExprKind::FieldAccess { object, .. } => {
                self.collect_global_expr_deps(object, out);
            }
            HirExprKind::SliceAccess { object, index } => {
                self.collect_global_expr_deps(object, out);
                self.collect_global_expr_deps(index, out);
            }
            HirExprKind::StructInit { fields, .. } => {
                for field in fields {
                    self.collect_global_expr_deps(&field.value, out);
                }
            }
            HirExprKind::ArrayInit { elements } => {
                for element in elements {
                    self.collect_global_expr_deps(element, out);
                }
            }
            HirExprKind::ArrayRepeatInit { element, len } => {
                self.collect_global_expr_deps(element, out);
                self.collect_global_expr_deps(len, out);
            }
            HirExprKind::Block { stmts, trailing } => {
                for stmt in stmts {
                    self.collect_global_stmt_deps(stmt, out);
                }
                if let Some(trailing) = trailing {
                    self.collect_global_expr_deps(trailing, out);
                }
            }
            HirExprKind::Closure { def, .. } => {
                if let Some(body) = &def.body {
                    self.collect_global_stmt_deps(body, out);
                }
            }
            HirExprKind::Literal(_)
            | HirExprKind::GenericParamRef(_)
            | HirExprKind::Switch
            | HirExprKind::Type(_)
            | HirExprKind::Error => {}
        }
    }

    fn collect_global_stmt_deps(&self, stmt: &HirStmt, out: &mut Vec<DefId>) {
        match &stmt.kind {
            HirStmtKind::Let { value, .. } => {
                if let Some(value) = value {
                    self.collect_global_expr_deps(value, out);
                }
            }
            HirStmtKind::Assign { object, value } => {
                self.collect_global_expr_deps(object, out);
                self.collect_global_expr_deps(value, out);
            }
            HirStmtKind::CompoundAssign { object, value, .. } => {
                self.collect_global_expr_deps(object, out);
                self.collect_global_expr_deps(value, out);
            }
            HirStmtKind::Return { value } => {
                if let Some(value) = value {
                    self.collect_global_expr_deps(value, out);
                }
            }
            HirStmtKind::While { condition, block } => {
                self.collect_global_expr_deps(condition, out);
                self.collect_global_stmt_deps(block, out);
            }
            HirStmtKind::For {
                iterator, block, ..
            } => {
                self.collect_global_expr_deps(iterator, out);
                self.collect_global_stmt_deps(block, out);
            }
            HirStmtKind::Expr(expr) => self.collect_global_expr_deps(expr, out),
            HirStmtKind::Break
            | HirStmtKind::Continue
            | HirStmtKind::FnDecl(_)
            | HirStmtKind::Error => {}
        }
    }

    /// Ensures a concrete `drop` MIR function exists for every struct type
    /// that implements the `Drop` interface and shows up in the lowered
    /// program. `drop(%x)` statements refer to that function, so it must be
    /// registered even when nothing calls it explicitly.
    fn register_drop_functions(&mut self) {
        let Some(drop_iface) = self.find_interface_def("Drop") else {
            return;
        };

        let candidate_types: Vec<(TypeId, DefId, Vec<TypeId>)> = self
            .program
            .functions
            .values()
            .flat_map(|func| func.locals.iter().map(|local| local.ty))
            .filter_map(|ty| match self.typecheck.interner.get(ty).clone() {
                Type::Struct {
                    def_id,
                    generic_args,
                } => Some((ty, def_id, generic_args)),
                _ => None,
            })
            .collect();

        let mut seen: HashSet<(DefId, Vec<TypeId>)> = HashSet::new();
        for (ty, struct_def, generic_args) in candidate_types {
            let is_new = seen.insert((struct_def, generic_args.clone()));
            if !is_new {
                continue;
            }

            let drop_impl = self
                .typecheck
                .struct_info
                .get(&struct_def)
                .is_some_and(|info| info.capabalities.has_explicit_drop);
            if !drop_impl {
                continue;
            }

            let Some(methods) = self.resolution.impls.get(&(struct_def, drop_iface)) else {
                continue;
            };
            let Some(drop_method) = methods.iter().find(|def| {
                let name = &self.resolution.defs[*def].name;
                self.rodeo.borrow().resolve(name) == "drop"
            }) else {
                continue;
            };

            let mono_id = self.monomorphize_fn(*drop_method, generic_args, Some(struct_def), &[]);
            if let Some(func) = self.program.functions.get_mut(&mono_id) {
                func.is_drop_impl = true;
            }
            self.program.drop_functions.insert(ty, mono_id);
        }
    }

    /// Registers the layout of every `FatFn` type showing up among the
    /// lowered locals. A fat value *is* its environment: a `Closure` body's
    /// layout is the env struct's capture fields, a `Pointer` body holds one
    /// fn pointer. Building a fat value registers its own layout, but a value
    /// that merely flows through a parameter or a call slot never gets one
    /// built, and field access on it needs the layout too.
    fn register_reachable_fat_layouts(&mut self) {
        let fat_types: Vec<TypeId> = self
            .program
            .functions
            .values()
            .flat_map(|func| func.locals.iter().map(|local| local.ty))
            .filter(|&ty| matches!(self.typecheck.interner.get(ty), Type::FatFn { .. }))
            .collect::<HashSet<TypeId>>()
            .into_iter()
            .collect();

        for fat_ty in fat_types {
            if self.program.struct_layouts.contains_key(&fat_ty) {
                continue;
            }
            match self.typecheck.interner.get(fat_ty).clone() {
                Type::FatFn {
                    body: FatFnBody::Closure { env, .. },
                    ..
                } => {
                    if let Type::Struct { def_id, .. } = self.typecheck.interner.get(env).clone() {
                        self.register_fat_layout(fat_ty, def_id);
                    }
                }
                Type::FatFn {
                    body: FatFnBody::Pointer { pointee },
                    ..
                } => {
                    self.program.struct_layouts.insert(
                        fat_ty,
                        StructLayout {
                            def_id: CLOSURE_FAT_DEF,
                            generic_args: vec![],
                            fields: vec![StructFieldLayout {
                                def_id: CLOSURE_FAT_FN_FIELD,
                                ty: pointee,
                            }],
                        },
                    );
                }
                _ => {}
            }
        }
    }

    /// Registers the drop function of every `FnOnce` fat type showing up
    /// among the lowered locals. The value owns its captured environment
    /// inline, so its death tears the captured (non-Copy) values down —
    /// nothing is ever freed. `Fn` values hold only Copy captures and need
    /// no drop.
    fn register_fat_drop_functions(&mut self) {
        let fat_types: Vec<TypeId> = self
            .program
            .functions
            .values()
            .flat_map(|func| func.locals.iter().map(|local| local.ty))
            .filter(|&ty| {
                matches!(
                    self.typecheck.interner.get(ty),
                    Type::FatFn {
                        once: true,
                        body: FatFnBody::Closure { .. },
                        ..
                    }
                )
            })
            .collect::<HashSet<TypeId>>()
            .into_iter()
            .collect();

        for fat_ty in fat_types {
            if self.program.drop_functions.contains_key(&fat_ty) {
                continue;
            }
            let id = self.fat_drop_function(fat_ty);
            self.program.drop_functions.insert(fat_ty, id);
        }
    }

    /// Returns (synthesizing on first use) the drop function of a fat type:
    /// teardown of the captured values. Consuming `FnOnce` calls reuse it to
    /// release the captures right after the call.
    fn fat_drop_function(&mut self, fat_ty: TypeId) -> MirFunctionId {
        if let Some(&id) = self.fat_drop_cache.get(&fat_ty) {
            return id;
        }
        let id = self.synthesize_fat_drop_function(fat_ty);
        self.fat_drop_cache.insert(fat_ty, id);
        id
    }

    /// Builds the drop function of a `FnOnce` fat type: the value is passed
    /// by value and `Drop` statements tear each teardown-needing capture
    /// down (codegen expands them recursively). Nothing is freed — the
    /// captures live inside the value itself.
    fn synthesize_fat_drop_function(&mut self, fat_ty: TypeId) -> MirFunctionId {
        let Type::FatFn {
            body: FatFnBody::Closure { env, .. },
            ..
        } = self.typecheck.interner.get(fat_ty).clone()
        else {
            unreachable!("fat drop requires a closure-body fat type");
        };

        if let Type::Struct { def_id, .. } = self.typecheck.interner.get(env).clone() {
            self.register_fat_layout(fat_ty, def_id);
        }

        let id = self.mono_cache.fresh_id();
        self.set_function_name(id, format!("$fatdrop#{}", id.0));

        let void_ty = self.typecheck.interner.intern(Type::Void);

        let mut func = MirFunction {
            source_def: CLOSURE_FAT_DEF,
            mono_args: Vec::new(),
            locals: Vec::new(),
            blocks: Vec::new(),
            params: Vec::new(),
            entry_block: BlockId(0),
            ret_ty: void_ty,
            is_drop_impl: true,
        };
        func.new_block(); // block 0 (entry)
        let bb1 = func.new_block();

        let self_param = func.new_local(LocalDecl {
            ty: fat_ty,
            mutability: Mutability::Mut,
            kind: LocalKind::Param,
            name: None,
            source: None,
        });
        func.params.push(self_param);

        for field in self.env_struct_fields(env) {
            if !self.type_needs_teardown(field.field_ty) {
                continue;
            }
            let field_place = Place::from_local(self_param).field(field.field_def);
            func.blocks[0]
                .statements
                .push(MirStatement::Drop(field_place));
        }

        func.blocks[0].terminator = Terminator::Goto(bb1);
        func.blocks[bb1.0 as usize].terminator =
            Terminator::Return(Operand::Constant(ConstValue::Void, None));

        self.program.functions.insert(id, func);
        id
    }

    /// Field infos of a synthesized env struct type.
    fn env_struct_fields(&self, env_ty: TypeId) -> Vec<zeen_types::StructFieldInfo> {
        match self.typecheck.interner.get(env_ty).clone() {
            Type::Struct { def_id, .. } => self
                .typecheck
                .struct_info
                .get(&def_id)
                .map(|info| info.fields.clone())
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Whether a value of this type needs teardown when its owner dies:
    /// structs with a `Drop` implementation (or nested teardown-needing
    /// fields), fat closure values, and arrays of such. Env structs are
    /// never generic, so no substitution is needed here.
    fn type_needs_teardown(&self, ty: TypeId) -> bool {
        match self.typecheck.interner.get(ty).clone() {
            Type::Struct { def_id, .. } => {
                let Some(info) = self.typecheck.struct_info.get(&def_id) else {
                    return false;
                };
                info.capabalities.has_explicit_drop
                    || info
                        .fields
                        .iter()
                        .any(|f| self.type_needs_teardown(f.field_ty))
            }
            Type::FatFn { .. } => true,
            Type::Array { element, .. } => self.type_needs_teardown(element),
            _ => false,
        }
    }

    fn find_interface_def(&self, interface_name: &str) -> Option<DefId> {
        for (def, info) in &self.resolution.defs {
            if matches!(info.kind, DefKind::Interface)
                && self.rodeo.borrow().resolve(&info.name) == interface_name
            {
                return Some(*def);
            }
        }
        None
    }

    fn expr_type(&mut self, fb: &FnBuilder, expr: &HirExpr) -> TypeId {
        // A fat-annotated parameter is erased in the signature but stores a
        // concrete closure type in this monomorphized copy: rewrite reads of
        // it to the bound type.
        if let HirExprKind::VarRef(def_id) = &expr.kind
            && let Some((_, bound_ty)) = fb.fat_bindings.iter().find(|(d, _)| d == def_id)
        {
            return *bound_ty;
        }

        let raw = self
            .typecheck
            .expr_types
            .get(&expr.id)
            .copied()
            .expect("unrecorded HIR expr after Typechecker");
        self.substitute_fn_type(fb, raw)
    }

    fn substitute_fn_type(&mut self, fb: &FnBuilder, ty: TypeId) -> TypeId {
        if fb.bindings.is_empty() {
            ty
        } else {
            zeen_types::substitute_generics(&mut self.typecheck.interner, ty, &fb.bindings)
        }
    }

    fn substitute_generic_args(&mut self, fb: &FnBuilder, args: &[TypeId]) -> Vec<TypeId> {
        args.iter()
            .map(|&t| self.substitute_fn_type(fb, t))
            .collect()
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

    fn is_float_ty(&self, ty: TypeId) -> bool {
        matches!(
            self.typecheck.interner.get(ty),
            Type::Builtin(zeen_ast::types::BuiltinType::f32 | zeen_ast::types::BuiltinType::f64)
                | Type::FloatLiteral
        )
    }

    fn mir_type_is_copy(&self, ty: TypeId) -> bool {
        match self.typecheck.interner.get(ty).clone() {
            Type::Builtin(_)
            | Type::IntLiteral
            | Type::FloatLiteral
            | Type::Enum { .. }
            | Type::Pointer { .. }
            | Type::ManyPointer { .. }
            | Type::Fn { .. }
            | Type::Void
            | Type::Never
            | Type::Error => true,

            // `Fn` closure values (all-Copy captures or none) are Copy: the
            // inline environment is duplicated with the value. `FnOnce` owns
            // a non-Copy capture, so it is move-only.
            Type::FatFn { once, .. } => !once,

            Type::Struct { def_id, .. } => self
                .typecheck
                .struct_info
                .get(&def_id)
                .map(|info| info.capabalities.is_copy)
                .unwrap_or(false),

            Type::Array { element, .. } => self.mir_type_is_copy(element),

            Type::Slice { .. } => true,

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

    /// Register layouts for every user-defined struct (skipping core files) so that
    /// structs which are never referenced by any lowered function still get printed.
    fn register_user_struct_layouts(&mut self, resolution: &ResolutionResult) {
        let mut struct_defs: Vec<DefId> = resolution
            .defs
            .iter()
            .filter_map(|(def_id, info)| {
                if !matches!(info.kind, DefKind::Struct)
                    || info.span.src().name().starts_with("core.")
                {
                    return None;
                }

                Some(*def_id)
            })
            .collect();

        struct_defs.sort_by_key(|def_id| def_id.0);

        for struct_def in struct_defs {
            let generic_args: Vec<TypeId> = self
                .typecheck
                .struct_generics
                .get(&struct_def)
                .map(|generics| {
                    generics
                        .iter()
                        .map(|&generic| self.typecheck.interner.intern(Type::GenericParam(generic)))
                        .collect()
                })
                .unwrap_or_default();

            let struct_ty = self.typecheck.interner.intern(Type::Struct {
                def_id: struct_def,
                generic_args,
            });

            self.register_struct_layout(struct_ty, struct_def);
        }
    }

    fn register_struct_layout(&mut self, ty: TypeId, struct_def: DefId) {
        if self.program.struct_layouts.contains_key(&ty) {
            return;
        }

        let generic_args = match self.typecheck.interner.get(ty).clone() {
            Type::Struct { generic_args, .. } => generic_args,
            _ => return,
        };

        let struct_generics = self
            .typecheck
            .struct_generics
            .get(&struct_def)
            .cloned()
            .unwrap_or_default();
        let bindings: HashMap<DefId, TypeId> = struct_generics
            .iter()
            .copied()
            .zip(generic_args.iter().copied())
            .collect();

        let Some(info) = self.typecheck.struct_info.get(&struct_def).cloned() else {
            return;
        };

        let fields: Vec<StructFieldLayout> = info
            .fields
            .iter()
            .map(|f| StructFieldLayout {
                def_id: f.field_def,
                ty: zeen_types::substitute_generics(
                    &mut self.typecheck.interner,
                    f.field_ty,
                    &bindings,
                ),
            })
            .collect();

        self.program.struct_layouts.insert(
            ty,
            StructLayout {
                def_id: struct_def,
                generic_args,
                fields,
            },
        );
    }

    /// Registers a monomorphized `Slice[T]` as a synthetic struct layout so the
    /// MIR printer can show it and indexing/len projections have a home. The
    /// reserved `DefId`s stand in for the (never-user-visible) `ptr`/`len`
    /// fields.
    fn register_slice_layout(&mut self, ty: TypeId) {
        if self.program.struct_layouts.contains_key(&ty) {
            return;
        }

        let typecheck = &mut self.typecheck;
        let Type::Slice { element, is_const } = typecheck.interner.get(ty).clone() else {
            return;
        };

        let ptr_ty = typecheck.interner.intern(Type::ManyPointer {
            inner: element,
            is_const,
        });
        let len_ty = typecheck
            .interner
            .intern(Type::Builtin(zeen_ast::types::BuiltinType::usize));

        self.program.struct_layouts.insert(
            ty,
            StructLayout {
                def_id: SLICE_STRUCT_DEF,
                generic_args: vec![element],
                fields: vec![
                    StructFieldLayout {
                        def_id: SLICE_PTR_FIELD,
                        ty: ptr_ty,
                    },
                    StructFieldLayout {
                        def_id: SLICE_LEN_FIELD,
                        ty: len_ty,
                    },
                ],
            },
        );
    }

    /// Registers the layout of a fat closure value: the value *is* its
    /// environment, so the layout is the env struct's capture fields.
    fn register_fat_layout(&mut self, fat_ty: TypeId, env_def: DefId) {
        if self.program.struct_layouts.contains_key(&fat_ty) {
            return;
        }

        let env_ty = self.typecheck.interner.intern(Type::Struct {
            def_id: env_def,
            generic_args: Vec::new(),
        });
        self.register_struct_layout(env_ty, env_def);

        let fields = self
            .typecheck
            .struct_info
            .get(&env_def)
            .map(|info| {
                info.fields
                    .iter()
                    .map(|f| StructFieldLayout {
                        def_id: f.field_def,
                        ty: f.field_ty,
                    })
                    .collect()
            })
            .unwrap_or_default();

        self.program.struct_layouts.insert(
            fat_ty,
            StructLayout {
                def_id: CLOSURE_FAT_DEF,
                generic_args: vec![],
                fields,
            },
        );
    }

    /// Builds a fat closure value of type `fat_ty` and returns it as a moved
    /// operand. The value *is* the captured environment: an inline struct of
    /// the captured values, with `closure_def` naming the function its calls
    /// dispatch to.
    fn build_fat_envelope(
        &mut self,
        fb: &mut FnBuilder,
        block: BlockId,
        fat_ty: TypeId,
        closure_def: DefId,
        source_expr: &HirExpr,
    ) -> (BlockId, Operand) {
        let captures = self
            .resolution
            .closure_captures
            .get(&closure_def)
            .cloned()
            .unwrap_or_default();

        let env_def = closure_struct_def(closure_def);
        self.register_fat_layout(fat_ty, env_def);

        // Materialize each captured value on its own before grouping them,
        // so reads happen in source order.
        let mut capture_ops = Vec::with_capacity(captures.len());
        for captured in &captures {
            let local = *fb
                .locals_by_def
                .get(captured)
                .expect("captured value must be a local of the enclosing frame");
            let raw_ty = self
                .typecheck
                .def_types
                .get(captured)
                .copied()
                .expect("captured value must have a type");
            let ty = self.substitute_fn_type(fb, raw_ty);
            let place = Place::from_local(local);
            capture_ops.push(self.place_to_operand(place, ty, None));
        }

        let temp = fb.new_temp(fat_ty);
        fb.push_stmt(
            block,
            MirStatement::Assign {
                place: Place::from_local(temp),
                rvalue: Rvalue::Aggregate {
                    kind: AggregateKind::Struct(env_def),
                    operands: capture_ops,
                },
                source: Some(source_expr.source.clone()),
            },
        );

        (
            block,
            Operand::Move(Place::from_local(temp), Some(source_expr.source.clone())),
        )
    }

    /// Registers slice layouts for every `Slice[T]` reachable from a struct
    /// field. Codegen needs a `{ ptr, len }` body for any slice type that
    /// shows up as a struct field — `register_slice_layout` alone only sees
    /// slice-typed locals, so a struct holding a slice (even one pointing at
    /// static string data) used to crash codegen.
    fn register_reachable_slice_layouts(&mut self) {
        let mut visited: HashSet<TypeId> = HashSet::new();
        let struct_keys: Vec<TypeId> = self.program.struct_layouts.keys().copied().collect();
        for layout_ty in struct_keys {
            let fields: Vec<TypeId> = self.program.struct_layouts[&layout_ty]
                .fields
                .iter()
                .map(|f| f.ty)
                .collect();
            for field_ty in fields {
                self.register_slice_layouts_in_type(field_ty, &mut visited);
            }
        }

        // Array-typed locals (e.g. `[N][]const char`) never hit
        // `register_slice_layout` directly, but their element slices need a
        // layout all the same.
        let local_tys: Vec<TypeId> = self
            .program
            .functions
            .values()
            .flat_map(|f| f.locals.iter().map(|l| l.ty))
            .collect();
        for ty in local_tys {
            self.register_slice_layouts_in_type(ty, &mut visited);
        }
    }

    fn register_slice_layouts_in_type(&mut self, ty: TypeId, visited: &mut HashSet<TypeId>) {
        if !visited.insert(ty) {
            return;
        }
        match self.typecheck.interner.get(ty).clone() {
            Type::Slice { element, .. } => {
                self.register_slice_layout(ty);
                self.register_slice_layouts_in_type(element, visited);
            }
            Type::Array { element, .. } => self.register_slice_layouts_in_type(element, visited),
            Type::Struct { .. } => {
                let fields: Vec<TypeId> = self
                    .program
                    .struct_layouts
                    .get(&ty)
                    .map(|layout| layout.fields.iter().map(|f| f.ty).collect())
                    .unwrap_or_default();
                for field_ty in fields {
                    self.register_slice_layouts_in_type(field_ty, visited);
                }
            }
            _ => {}
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
                let ty = self.expr_type(fb, expr);
                (
                    block,
                    Operand::Constant(self.lower_literal(lit, ty), Some(expr.source.clone())),
                )
            }

            HirExprKind::VarRef(def_id) | HirExprKind::SelfValue(def_id) => {
                if let Some(global_id) = self.globals_by_def.get(def_id) {
                    return (
                        block,
                        Operand::Copy(Place::global(*global_id), Some(expr.source.clone())),
                    );
                }

                let expr_ty = self.expr_type(fb, expr);

                if matches!(&expr.kind, HirExprKind::VarRef(_))
                    && matches!(
                        self.resolution.defs.get(def_id).map(|info| &info.kind),
                        Some(DefKind::Function)
                    )
                {
                    // A static function coerced into a fat slot becomes a
                    // closure value with an empty inline env whose calls
                    // dispatch directly to the function.
                    if matches!(self.typecheck.interner.get(expr_ty), Type::FatFn { .. }) {
                        return self.build_fat_envelope(fb, block, expr_ty, *def_id, expr);
                    }

                    let mir_id = self.monomorphize_fn(*def_id, Vec::new(), None, &[]);
                    return (
                        block,
                        Operand::Constant(ConstValue::Fn(mir_id), Some(expr.source.clone())),
                    );
                }

                let place = fb.place_for_def(*def_id).unwrap_or_else(|| {
                    panic!("HIR DefId {:?} has no MIR local", def_id);
                });
                let ty = self.place_type(fb, &place);

                // A basic fn value read from a variable coerces into a fat
                // slot at runtime: wrap the pointer into a one-field fat
                // value, which the call dispatches through indirectly.
                if matches!(self.typecheck.interner.get(expr_ty), Type::FatFn { .. })
                    && !matches!(self.typecheck.interner.get(ty), Type::FatFn { .. })
                {
                    let inner = self.place_to_operand(place.clone(), ty, None);
                    let temp = fb.new_temp(expr_ty);
                    fb.push_stmt(
                        block,
                        MirStatement::Assign {
                            place: Place::from_local(temp),
                            rvalue: Rvalue::Aggregate {
                                kind: AggregateKind::Struct(CLOSURE_FAT_DEF),
                                operands: vec![inner],
                            },
                            source: Some(expr.source.clone()),
                        },
                    );
                    return (
                        block,
                        Operand::Move(Place::from_local(temp), Some(expr.source.clone())),
                    );
                }

                let operand = self.place_to_operand(place, ty, Some(expr.source.clone()));
                (block, operand)
            }

            HirExprKind::Binary { lhs, rhs, op } => {
                if let Some(op_res) = self.typecheck.operator_resolutions.get(&expr.id).cloned() {
                    let (block, rhs_op) = self.lower_expr_to_operand(fb, rhs, block);
                    let rhs_ty = self.expr_type(fb, rhs);
                    let result_ty = self.expr_type(fb, expr);
                    let (block, call_op) = self.lower_operator_method_call_with_extra_args(
                        fb,
                        lhs,
                        &[(rhs_op, rhs_ty)],
                        &op_res,
                        block,
                        result_ty,
                    );

                    // `!=` dispatches to `Eq.eq` and negates the result.
                    if matches!(op, BinaryOp::Ne) {
                        let temp = fb.new_temp(result_ty);
                        fb.push_stmt(
                            block,
                            MirStatement::Assign {
                                place: Place::from_local(temp),
                                rvalue: Rvalue::UnaryOp {
                                    op: UnaryOp::Not,
                                    operand: call_op,
                                },
                                source: Some(expr.source.clone()),
                            },
                        );
                        return (
                            block,
                            Operand::Move(Place::from_local(temp), Some(expr.source.clone())),
                        );
                    }

                    return (block, call_op);
                }

                let (block, lhs_op) = self.lower_expr_to_operand(fb, lhs, block);
                let (block, rhs_op) = self.lower_expr_to_operand(fb, rhs, block);

                let result_ty = self.expr_type(fb, expr);

                let rhs_ty = self.expr_type(fb, rhs);
                let is_float_div = self.is_float_ty(rhs_ty);

                // `/` and `%` panic on a zero divisor in Debug builds; the
                // divisor is materialized into a local so the guard can read it
                // without moving the value the division itself uses. Floats are
                // excluded: IEEE-754 division by zero yields inf/nan.
                let (block, rhs_op) =
                    if matches!(op, BinaryOp::Div | BinaryOp::Mod) && !is_float_div {
                        let rhs_local = self.operand_to_local(fb, rhs_op, rhs_ty, block);
                        let block = self.lower_div_zero_check(
                            fb,
                            block,
                            rhs_local,
                            rhs_ty,
                            Some(expr.source.clone()),
                        );
                        (
                            block,
                            Operand::Copy(Place::from_local(rhs_local), Some(expr.source.clone())),
                        )
                    } else {
                        (block, rhs_op)
                    };

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
                        source: Some(expr.source.clone()),
                    },
                );

                (
                    block,
                    Operand::Move(Place::from_local(temp), Some(expr.source.clone())),
                )
            }

            HirExprKind::Unary {
                expr: inner,
                op: UnaryOp::AddrOf,
            } => {
                let result_ty = self.expr_type(fb, expr);

                // `&array` lowers to a slice: build the `{ ptr, len }` fat
                // pointer as a real slice aggregate instead of shoving a bare
                // `addr_of` into a slice-typed local.
                if let Type::Slice { element, is_const } =
                    self.typecheck.interner.get(result_ty).clone()
                {
                    let inner_ty = self.expr_type(fb, inner);
                    let len_val = match self.typecheck.interner.get(inner_ty).clone() {
                        Type::Array { len: Some(len), .. } => len,
                        _ => panic!("&slice: inner operand must be a fixed array"),
                    };

                    let ptr_ty = self.typecheck.interner.intern(Type::ManyPointer {
                        inner: element,
                        is_const,
                    });
                    let ptr_temp = fb.new_temp(ptr_ty);

                    // `&` needs a place to point at. When the operand is not an
                    // lvalue (e.g. an array literal), materialize it into a temp
                    // local first so we can take its address instead of panicking.
                    let (block, inner_place) = if self.expr_is_place(inner) {
                        self.lower_expr_to_place(fb, inner, block)
                    } else {
                        let (block, inner_op) = self.lower_expr_to_operand(fb, inner, block);
                        let temp = fb.new_temp(inner_ty);
                        fb.push_stmt(
                            block,
                            MirStatement::Assign {
                                place: Place::from_local(temp),
                                rvalue: Rvalue::Use(inner_op),
                                source: Some(inner.source.clone()),
                            },
                        );
                        (block, Place::from_local(temp))
                    };

                    fb.push_stmt(
                        block,
                        MirStatement::Assign {
                            place: Place::from_local(ptr_temp),
                            rvalue: Rvalue::Ref {
                                place: inner_place,
                                is_const,
                            },
                            source: Some(expr.source.clone()),
                        },
                    );

                    let len_operand = Operand::Constant(ConstValue::Int(len_val as i128), None);

                    let temp = fb.new_temp(result_ty);
                    fb.push_stmt(
                        block,
                        MirStatement::Assign {
                            place: Place::from_local(temp),
                            rvalue: Rvalue::Aggregate {
                                kind: AggregateKind::Slice,
                                operands: vec![
                                    Operand::Move(Place::from_local(ptr_temp), None),
                                    len_operand,
                                ],
                            },
                            source: Some(expr.source.clone()),
                        },
                    );

                    return (
                        block,
                        Operand::Move(Place::from_local(temp), Some(expr.source.clone())),
                    );
                }

                let is_const = match self.typecheck.interner.get(result_ty).clone() {
                    Type::Pointer { is_const, .. } => is_const,
                    _ => false,
                };

                // Type of the pointee. Use the pointer's own inner type so a
                // literal operand is pinned to the concrete pointee (`&123`
                // in a `*i64` context materializes an `i64` temp, not an `i32`
                // defaulted one).
                let inner_ty = match self.typecheck.interner.get(result_ty).clone() {
                    Type::Pointer { inner, .. } => inner,
                    _ => self.expr_type(fb, inner),
                };

                // `&` needs a place to point at. When the operand is not an
                // lvalue (e.g. `&123`, `&(a + b)`), materialize it into a temp
                // local first so we can take its address instead of panicking.
                let (block, inner_place) = if self.expr_is_place(inner) {
                    self.lower_expr_to_place(fb, inner, block)
                } else {
                    let (block, inner_op) = self.lower_expr_to_operand(fb, inner, block);
                    let temp = fb.new_temp(inner_ty);
                    fb.push_stmt(
                        block,
                        MirStatement::Assign {
                            place: Place::from_local(temp),
                            rvalue: Rvalue::Use(inner_op),
                            source: Some(inner.source.clone()),
                        },
                    );
                    (block, Place::from_local(temp))
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
                        source: Some(expr.source.clone()),
                    },
                );

                (
                    block,
                    Operand::Move(Place::from_local(temp), Some(expr.source.clone())),
                )
            }

            HirExprKind::Unary { expr: inner, op } => {
                if let Some(op_res) = self.typecheck.operator_resolutions.get(&expr.id).cloned() {
                    let result_ty = self.expr_type(fb, expr);
                    return self.lower_operator_method_call(fb, inner, &op_res, block, result_ty);
                }

                let (block, inner_op) = self.lower_expr_to_operand(fb, inner, block);

                let result_ty = self.expr_type(fb, expr);
                let temp = fb.new_temp(result_ty);

                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place: Place::from_local(temp),
                        rvalue: Rvalue::UnaryOp {
                            op: *op,
                            operand: inner_op,
                        },
                        source: Some(expr.source.clone()),
                    },
                );

                (
                    block,
                    Operand::Move(Place::from_local(temp), Some(expr.source.clone())),
                )
            }

            HirExprKind::SliceAccess { object, index } => {
                if let Some(op_res) = self.typecheck.operator_resolutions.get(&expr.id).cloned() {
                    let (block, index_operand) = self.lower_expr_to_operand(fb, index, block);
                    let index_ty = self.expr_type(fb, index);
                    let result_ty = self.expr_type(fb, expr);

                    return self.lower_operator_method_call_with_extra_args(
                        fb,
                        object,
                        &[(index_operand, index_ty)],
                        &op_res,
                        block,
                        result_ty,
                    );
                }

                let obj_ty = self.expr_type(fb, object);
                let (block, obj_place) = self.lower_expr_to_place_or_temp(fb, object, block);
                let (block, index_operand) = self.lower_expr_to_operand(fb, index, block);

                let index_local = {
                    let idx_ty = self.expr_type(fb, index);
                    self.operand_to_local(fb, index_operand, idx_ty, block)
                };

                let block = self.lower_bounds_check(
                    fb,
                    block,
                    &obj_place,
                    obj_ty,
                    index_local,
                    Some(expr.source.clone()),
                );

                let elem_place = match self.typecheck.interner.get(obj_ty).clone() {
                    Type::Array { .. } | Type::ManyPointer { .. } => obj_place.index(index_local),
                    Type::Slice { .. } => {
                        let mut ptr_place = obj_place;
                        ptr_place.projection.push(PlaceElem::Field(SLICE_PTR_FIELD));
                        ptr_place.index(index_local)
                    }
                    _ => unreachable!(),
                };

                let ty = self.expr_type(fb, expr);

                (
                    block,
                    self.place_to_operand(elem_place, ty, Some(expr.source.clone())),
                )
            }

            HirExprKind::FieldAccess { object, field } => {
                // C-like enum variant access, e.g. `Color.Red`: the whole
                // expression is just a constant, not a real place.
                if let HirExprKind::VarRef(enum_def) = &object.kind
                    && matches!(
                        self.resolution.defs.get(enum_def).map(|info| &info.kind),
                        Some(DefKind::Enum)
                    )
                {
                    let variant_def = self.field_resolution(expr.id);
                    let index = self
                        .typecheck
                        .enum_variants
                        .get(enum_def)
                        .and_then(|variants| variants.iter().position(|&v| Some(v) == variant_def))
                        .unwrap_or(0) as i64;
                    return (
                        block,
                        Operand::Constant(
                            ConstValue::Int(index as i128),
                            Some(expr.source.clone()),
                        ),
                    );
                }

                // `arr.len` on a fixed array is a compile-time constant: arrays
                // carry no runtime length field, so lower it to a constant
                // instead of projecting into storage.
                let obj_ty = self.expr_type(fb, object);
                if let Type::Array { len: Some(len), .. } =
                    self.typecheck.interner.get(obj_ty).clone()
                    && self.rodeo.borrow().resolve(&field.0) == "len"
                {
                    return (
                        block,
                        Operand::Constant(ConstValue::Int(len as i128), Some(expr.source.clone())),
                    );
                }

                let (block, place) = self.lower_expr_to_place(fb, expr, block);
                let ty = self.expr_type(fb, expr);
                (
                    block,
                    self.place_to_operand(place, ty, Some(expr.source.clone())),
                )
            }

            HirExprKind::StructInit { fields, .. } => {
                let ty = self.expr_type(fb, expr);
                let struct_def = match self.typecheck.interner.get(ty).clone() {
                    Type::Struct { def_id, .. } => def_id,
                    wildcard => panic!("non-struct type in StructInit lowering: {:?}", wildcard),
                };

                self.register_struct_layout(ty, struct_def);

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
                        source: Some(expr.source.clone()),
                    },
                );

                (
                    block,
                    self.place_to_operand(Place::from_local(temp), ty, Some(expr.source.clone())),
                )
            }

            HirExprKind::ArrayInit { elements } => {
                let ty = self.expr_type(fb, expr);
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
                        source: Some(expr.source.clone()),
                    },
                );

                (
                    block,
                    self.place_to_operand(Place::from_local(temp), ty, Some(expr.source.clone())),
                )
            }

            HirExprKind::ArrayRepeatInit { element, len: _ } => {
                let ty = self.expr_type(fb, expr);
                let (block, elem_op) = self.lower_expr_to_operand(fb, element, block);

                let n = match self.typecheck.interner.get(ty).clone() {
                    Type::Array { len: Some(n), .. } => n as usize,
                    _ => 0,
                };

                let operands = vec![elem_op; n];
                let temp = fb.new_temp(ty);
                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place: Place::from_local(temp),
                        rvalue: Rvalue::Aggregate {
                            kind: AggregateKind::Array,
                            operands,
                        },
                        source: Some(expr.source.clone()),
                    },
                );

                (
                    block,
                    self.place_to_operand(Place::from_local(temp), ty, Some(expr.source.clone())),
                )
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

                let result_ty = self.expr_type(fb, expr);
                let has_else = else_block.is_some();

                if !has_else {
                    let join = fb.new_block();
                    fb.join_if_open(then_end, join);
                    fb.join_if_open(else_bb, join);
                    return (join, Operand::Constant(ConstValue::Void, None));
                }

                let (else_end, else_operand) =
                    self.lower_stmt_as_block_value(fb, else_block.as_ref().unwrap(), else_bb);
                let join = fb.new_block();

                let result_local = fb.new_temp(result_ty);

                if fb.block_is_open(then_end) {
                    fb.push_stmt(
                        then_end,
                        MirStatement::Assign {
                            place: Place::from_local(result_local),
                            rvalue: Rvalue::Use(then_operand),
                            source: Some(expr.source.clone()),
                        },
                    );
                    fb.set_terminator(then_end, Terminator::Goto(join));
                }

                if fb.block_is_open(else_end) {
                    fb.push_stmt(
                        else_end,
                        MirStatement::Assign {
                            place: Place::from_local(result_local),
                            rvalue: Rvalue::Use(else_operand),
                            source: Some(expr.source.clone()),
                        },
                    );
                    fb.set_terminator(else_end, Terminator::Goto(join));
                }

                (
                    join,
                    Operand::Move(Place::from_local(result_local), Some(expr.source.clone())),
                )
            }

            HirExprKind::Call { callee, args, .. } => {
                let call_id = expr.id;

                let Some(resolution) = self.call_resolution(call_id) else {
                    let result_ty = self.expr_type(fb, expr);
                    return self.lower_indirect_call(fb, callee, args, block, result_ty);
                };

                let fn_def = resolution.fn_def;
                let raw_args = resolution.generic_args.clone();
                let generic_args = self.substitute_generic_args(fb, &raw_args);

                let Some(hir_fn) = self.hir_fns_by_def.get(&fn_def).cloned() else {
                    unreachable!("must been recorded this table");
                };

                let method_ty = self.typecheck.def_types.get(&fn_def).copied().expect("...");
                let param_count = match self.typecheck.interner.get(method_ty).clone() {
                    Type::Fn { params, .. } => params.len(),
                    _ => 0,
                };
                let has_self_param = param_count == args.len() + 1;

                // `Fn`/`FnOnce`-typed parameters are erased in the signature:
                // this call instantiates the callee with the concrete closure
                // type of every fat argument, in fat-parameter order.
                let declared_params: Vec<TypeId> = match self.typecheck.interner.get(method_ty) {
                    Type::Fn { params, .. } => params.clone(),
                    _ => Vec::new(),
                };
                let substituted_params: Vec<TypeId> = declared_params
                    .iter()
                    .map(|&t| self.substitute_fn_type(fb, t))
                    .collect();
                let mut fat_args: Vec<TypeId> = Vec::new();
                for (offset, arg) in args.iter().enumerate() {
                    let param_ty = substituted_params
                        .get(offset + usize::from(has_self_param))
                        .copied()
                        .unwrap_or(self.typecheck.interner.error());
                    if matches!(
                        self.typecheck.interner.get(param_ty),
                        Type::FatFn {
                            body: FatFnBody::Bound,
                            ..
                        }
                    ) {
                        fat_args.push(self.expr_type(fb, arg));
                    }
                }

                let call_target =
                    self.resolve_call_target(fn_def, generic_args, &hir_fn, &fat_args);

                let mut arg_operands = Vec::with_capacity(args.len() + 1);

                if let HirExprKind::FieldAccess { object, .. } = &callee.kind
                    && has_self_param
                {
                    let (b, self_operand) = self.lower_receiver_operand(fb, object, fn_def, block);
                    block = b;
                    arg_operands.push(self_operand);
                }

                for arg in args.iter() {
                    let (b, op) = self.lower_expr_to_operand(fb, arg, block);
                    block = b;
                    arg_operands.push(op);
                }

                let ret_ty = self.expr_type(fb, expr);
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
                        source: Some(expr.source.clone()),
                    },
                );

                if is_diverging {
                    fb.set_terminator(next_block, Terminator::Unreachable);
                    (next_block, Operand::Constant(ConstValue::Void, None))
                } else {
                    (
                        next_block,
                        self.place_to_operand(dest_place, ret_ty, Some(expr.source.clone())),
                    )
                }
            }

            HirExprKind::MacroCall { kind, args } => match kind.0 {
                HirMacroKind::SizeOf | HirMacroKind::AlignOf => {
                    let target_ty = match &args[0].kind {
                        HirExprKind::Type(_) => self.expr_type(fb, &args[0]),
                        _ => panic!("@sizeof / @alignof arg must be a type expression"),
                    };

                    let result_ty = self.expr_type(fb, expr);
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
                            source: Some(expr.source.clone()),
                        },
                    );
                    (
                        block,
                        Operand::Move(Place::from_local(temp), Some(expr.source.clone())),
                    )
                }

                HirMacroKind::As => {
                    let target_ty = match &args[0].kind {
                        HirExprKind::Type(_) => self.expr_type(fb, &args[0]),
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
                            source: Some(expr.source.clone()),
                        },
                    );
                    (
                        block,
                        Operand::Move(Place::from_local(temp), Some(expr.source.clone())),
                    )
                }

                HirMacroKind::TypeName => {
                    let target_ty = match &args[0].kind {
                        HirExprKind::Type(_) => self.expr_type(fb, &args[0]),
                        _ => panic!("@sizeof / @alignof arg must be a type expression"),
                    };

                    let result_ty = self.expr_type(fb, expr);
                    let temp = fb.new_temp(result_ty);

                    let stringified_ty = self.typecheck.interner.display_type(
                        target_ty,
                        Rc::clone(&self.rodeo),
                        self.resolution,
                    );

                    let mut rodeo = self.rodeo.borrow_mut();
                    let stringified_spur = rodeo.get_or_intern(stringified_ty);
                    drop(rodeo);

                    let rvalue = Rvalue::Use(Operand::Constant(
                        ConstValue::Str(stringified_spur),
                        Some(expr.source.clone()),
                    ));

                    fb.push_stmt(
                        block,
                        MirStatement::Assign {
                            place: Place::from_local(temp),
                            rvalue,
                            source: Some(expr.source.clone()),
                        },
                    );
                    (
                        block,
                        Operand::Move(Place::from_local(temp), Some(expr.source.clone())),
                    )
                }

                HirMacroKind::Dbg if self.mode == CompilationMode::Release => {
                    let value = args
                        .first()
                        .expect("typechecker requires @dbg to have exactly one argument");
                    self.lower_expr_to_operand(fb, value, block)
                }

                HirMacroKind::Print
                | HirMacroKind::Println
                | HirMacroKind::Format
                | HirMacroKind::Dbg
                | HirMacroKind::Panic => {
                    self.lower_macro_call(fb, kind.0, args, expr.id, block, expr.source.clone())
                }

                HirMacroKind::Unreachable | HirMacroKind::Todo => {
                    self.lower_diverging_macro(fb, kind.0, block)
                }

                HirMacroKind::Uninit => (block, Operand::Constant(ConstValue::Void, None)),

                HirMacroKind::Unknown => panic!("unknown macro reached MIR lowering"),
            },

            HirExprKind::Block { stmts, trailing } => {
                fb.scope_stack.push(Vec::new());

                let mut cur = block;

                for stmt in stmts.iter() {
                    cur = self.lower_stmt(fb, stmt, cur);
                }

                let (cur, operand) = match trailing {
                    Some(t) => self.lower_expr_to_operand(fb, t, cur),
                    None => (cur, Operand::Constant(ConstValue::Void, None)),
                };

                let locals = fb.scope_stack.pop().unwrap();

                let (cur, operand) = match &operand {
                    Operand::Copy(place, _) | Operand::Move(place, _) => {
                        if locals.contains(&place.local) {
                            let ty = fb.func.local(place.local).ty;
                            let temp = fb.new_temp(ty);
                            fb.push_stmt(
                                cur,
                                MirStatement::Assign {
                                    place: Place::from_local(temp),
                                    rvalue: Rvalue::Use(operand),
                                    source: Some(expr.source.clone()),
                                },
                            );
                            (
                                cur,
                                Operand::Move(Place::from_local(temp), Some(expr.source.clone())),
                            )
                        } else {
                            (cur, operand)
                        }
                    }
                    Operand::Constant(_, _) => (cur, operand),
                };

                for local in locals.iter().rev() {
                    fb.push_stmt(cur, MirStatement::StorageDead(*local));
                }

                (cur, operand)
            }

            HirExprKind::GenericParamRef(_) => {
                self.errors.push(MirError::GenericParamNotValue {
                    src: expr.source.src(),
                    span: expr.source.span,
                });

                (block, Operand::Constant(ConstValue::NullPtr, None))
            }

            HirExprKind::Switch => unreachable!("not implemented in previous stages"),
            HirExprKind::Type(_) => unreachable!(),

            HirExprKind::Closure { def_id, .. } => {
                let closure_ty = self.expr_type(fb, expr);

                let mir_id = self.monomorphize_fn(*def_id, Vec::new(), None, &[]);

                match self.typecheck.interner.get(closure_ty).clone() {
                    // Zero-capture closure: a plain `fn` pointer.
                    Type::Fn { .. } => (
                        block,
                        Operand::Constant(ConstValue::Fn(mir_id), Some(expr.source.clone())),
                    ),

                    // Fat closure: the value is the captured environment
                    // itself; its calls dispatch directly to this literal's
                    // body.
                    Type::FatFn { .. } => {
                        self.build_fat_envelope(fb, block, closure_ty, *def_id, expr)
                    }

                    other => panic!("unexpected closure type: {other:?}"),
                }
            }

            HirExprKind::Error => unreachable!(),
        }
    }

    /// Whether `expr` can be lowered to a place by [`Self::lower_expr_to_place`].
    /// Mirrors the dispatch in that method.
    fn expr_is_place(&self, expr: &HirExpr) -> bool {
        match &expr.kind {
            HirExprKind::VarRef(_) | HirExprKind::SelfValue(_) => true,
            HirExprKind::FieldAccess { object, .. } => self.expr_is_place(object),
            HirExprKind::SliceAccess { object, .. } => self.expr_is_place(object),
            HirExprKind::Unary {
                expr: inner,
                op: UnaryOp::Deref,
            } => self.expr_is_place(inner),
            _ => false,
        }
    }

    /// Lower `expr` to a place, materializing it into a temp local first when
    /// it is not an lvalue (e.g. `get_obj().field` or `*get_ptr()`). The temp
    /// is a fresh storage cell that lives for the rest of the function, so the
    /// returned place is valid to read, write or take the address of.
    fn lower_expr_to_place_or_temp(
        &mut self,
        fb: &mut FnBuilder,
        expr: &HirExpr,
        block: BlockId,
    ) -> (BlockId, Place) {
        if self.expr_is_place(expr) {
            self.lower_expr_to_place(fb, expr, block)
        } else {
            let ty = self.expr_type(fb, expr);
            let (block, operand) = self.lower_expr_to_operand(fb, expr, block);
            let temp = fb.new_temp(ty);
            fb.push_stmt(
                block,
                MirStatement::Assign {
                    place: Place::from_local(temp),
                    rvalue: Rvalue::Use(operand),
                    source: Some(expr.source.clone()),
                },
            );
            (block, Place::from_local(temp))
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
                if let Some(global_id) = self.globals_by_def.get(def_id) {
                    return (block, Place::global(*global_id));
                }
                let place = fb
                    .place_for_def(*def_id)
                    .expect("undeclared local or capture");
                (block, place)
            }

            HirExprKind::FieldAccess { object, .. } => {
                let field_def = *self
                    .typecheck
                    .field_resolutions
                    .get(&expr.id)
                    .expect("unresolved shit");
                let obj_ty = self.expr_type(fb, object);
                let (block, obj_place) = self.lower_expr_to_place_or_temp(fb, object, block);

                // Field access through a pointer auto-derefs (`sf.x` where
                // `sf: *Foo`): the typechecker allows it, so insert the deref
                // projection explicitly instead of projecting into the pointer.
                let place = if matches!(self.typecheck.interner.get(obj_ty), Type::Pointer { .. }) {
                    obj_place.deref().field(field_def)
                } else {
                    obj_place.field(field_def)
                };

                (block, place)
            }

            HirExprKind::SliceAccess { object, index } => {
                let obj_ty = self.expr_type(fb, object);
                let (block, obj_place) = self.lower_expr_to_place_or_temp(fb, object, block);
                let (block, index_operand) = self.lower_expr_to_operand(fb, index, block);

                let index_local = match index_operand {
                    Operand::Copy(p, _) | Operand::Move(p, _) if p.projection.is_empty() => p.local,
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
                                source: Some(expr.source.clone()),
                            },
                        );

                        temp
                    }
                };

                let block = self.lower_bounds_check(
                    fb,
                    block,
                    &obj_place,
                    obj_ty,
                    index_local,
                    Some(expr.source.clone()),
                );

                (block, obj_place.index(index_local))
            }

            HirExprKind::Unary {
                expr: inner,
                op: UnaryOp::Deref,
            } => {
                // A deref through a struct's `Deref`/`DerefPtr` interface
                // dispatches to the method; the result is materialized into a
                // temp local. A `DerefPtr` result is a pointer into the
                // struct, so the place keeps dereferencing it (writes through
                // it reach the struct); a `Deref` result is the value itself.
                if let Some(op_res) = self.typecheck.operator_resolutions.get(&expr.id).cloned() {
                    // A `DerefPtr`-resolved deref (`*ref = v` in an assign)
                    // returns a pointer into the struct, so the place keeps
                    // dereferencing it; a `Deref`-resolved one yields the value
                    // itself. Decided from the method's own return type, since
                    // the recorded expr type is already unwrapped to the pointee.
                    let result_ty = self.expr_type(fb, expr);
                    let is_pointer = self
                        .typecheck
                        .def_types
                        .get(&op_res.method_def)
                        .is_some_and(|ty| {
                            matches!(
                                self.typecheck.interner.get(*ty),
                                Type::Fn { ret, .. }
                                    if matches!(
                                        self.typecheck.interner.get(*ret),
                                        Type::Pointer { .. } | Type::ManyPointer { .. }
                                    )
                            )
                        });

                    // The deref pointer is a pointer to the unwrapped pointee,
                    // rebuilt concretely so generics resolve to the right size.
                    let call_ty = if is_pointer {
                        self.typecheck.interner.intern(Type::Pointer {
                            inner: result_ty,
                            is_const: false,
                        })
                    } else {
                        result_ty
                    };

                    let (block, operand) =
                        self.lower_operator_method_call(fb, inner, &op_res, block, call_ty);

                    if is_pointer {
                        let ptr_local = self.operand_to_local(fb, operand, call_ty, block);
                        return (block, Place::from_local(ptr_local).deref());
                    }

                    let temp = self.operand_to_local(fb, operand, call_ty, block);
                    return (block, Place::from_local(temp));
                }

                let (block, inner_place) = self.lower_expr_to_place_or_temp(fb, inner, block);
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

    fn place_to_operand(&self, place: Place, ty: TypeId, source: Option<Source>) -> Operand {
        if self.mir_type_is_copy(ty) {
            Operand::Copy(place, source)
        } else {
            Operand::Move(place, source)
        }
    }

    fn place_type(&mut self, fb: &FnBuilder, place: &Place) -> TypeId {
        let (mut ty, start) = if let Some(PlaceElem::Global(id)) = place.projection.first() {
            (self.program.global_vars[id.0 as usize].ty, 1)
        } else {
            (fb.func.local(place.local).ty, 0)
        };

        for elem in &place.projection[start..] {
            ty = match elem {
                PlaceElem::Field(field_def) => self.struct_field_ty(ty, *field_def),
                PlaceElem::Index(_) => self.index_elem_ty(ty),
                PlaceElem::Deref => match self.typecheck.interner.get(ty).clone() {
                    Type::Pointer { inner, .. } | Type::ManyPointer { inner, .. } => inner,
                    other => panic!("dereferencing a non-pointer type: {other:?}"),
                },
                PlaceElem::Global(_) => unreachable!("global already consumed"),
            };
        }

        ty
    }

    /// Resolves the type of a struct field (potentially through generic
    /// arguments) from the typechecker's recorded layout.
    fn struct_field_ty(&mut self, ty: TypeId, field_def: DefId) -> TypeId {
        let Type::Struct {
            def_id,
            generic_args,
        } = self.typecheck.interner.get(ty).clone()
        else {
            // Fat closure values and their canonical fields resolve through the
            // registered fat layout.
            match self.typecheck.interner.get(ty).clone() {
                Type::FatFn { .. } => {
                    return self.program.struct_layouts[&ty]
                        .fields
                        .iter()
                        .find(|f| f.def_id == field_def)
                        .expect("field must be present in fat layout")
                        .ty;
                }
                other => panic!("projecting a field into a non-struct type: {other:?}"),
            }
        };

        let Some(info) = self.typecheck.struct_info.get(&def_id) else {
            panic!("struct layout missing for {def_id:?}");
        };
        let struct_generics = self
            .typecheck
            .struct_generics
            .get(&def_id)
            .cloned()
            .unwrap_or_default();
        let bindings: HashMap<DefId, TypeId> = struct_generics
            .iter()
            .copied()
            .zip(generic_args.iter().copied())
            .collect();

        let field = info
            .fields
            .iter()
            .find(|f| f.field_def == field_def)
            .expect("field must be present in struct");
        zeen_types::substitute_generics(&mut self.typecheck.interner, field.field_ty, &bindings)
    }

    fn index_elem_ty(&self, ty: TypeId) -> TypeId {
        match self.typecheck.interner.get(ty).clone() {
            Type::Array { element, .. } | Type::Slice { element, .. } => element,
            Type::ManyPointer { inner, .. } => inner,
            other => panic!("indexing a non-indexable type: {other:?}"),
        }
    }

    fn lower_receiver_operand(
        &mut self,
        fb: &mut FnBuilder,
        object: &HirExpr,
        method_def_id: DefId,
        block: BlockId,
    ) -> (BlockId, Operand) {
        let obj_ty = self.expr_type(fb, object);
        let (block, place) = self.lower_expr_to_place_or_temp(fb, object, block);

        self.lower_place_receiver_operand(
            fb,
            place,
            obj_ty,
            method_def_id,
            Some(object.source.clone()),
            block,
        )
    }

    fn lower_place_receiver_operand(
        &mut self,
        fb: &mut FnBuilder,
        place: Place,
        obj_ty: TypeId,
        method_def_id: DefId,
        source: Option<Source>,
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

        match expected_self_ty.map(|t| self.typecheck.interner.get(t).clone()) {
            Some(Type::Struct { .. }) | None => {
                (block, self.place_to_operand(place, obj_ty, source))
            }

            Some(Type::Pointer { is_const, .. }) => {
                match self.typecheck.interner.get(obj_ty).clone() {
                    Type::Pointer { .. } => (block, self.place_to_operand(place, obj_ty, source)),
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
                                source: source.clone(),
                            },
                        );
                        (block, Operand::Move(Place::from_local(temp), None))
                    }
                }
            }

            _ => (block, self.place_to_operand(place, obj_ty, source)),
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
            Operand::Copy(place, _) | Operand::Move(place, _) if place.projection.is_empty() => {
                place.local
            }
            _ => {
                let temp = fb.new_temp(ty);
                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place: Place::from_local(temp),
                        rvalue: Rvalue::Use(operand),
                        source: None,
                    },
                );
                temp
            }
        }
    }

    /// Inserts a `index < len` guard in front of an array/slice indexing
    /// access when compiling in Debug mode. An out-of-bounds index diverges
    /// into a `@panic` call that formats the bounds message. Raw pointers
    /// carry no length and are never checked. Returns the block the actual
    /// element access must be lowered into.
    ///
    /// `index_local` must be a plain local holding the (copyable) index value;
    /// it is only read here, never moved, so the element projection can use it
    /// again.
    fn lower_bounds_check(
        &mut self,
        fb: &mut FnBuilder,
        block: BlockId,
        obj_place: &Place,
        obj_ty: TypeId,
        index_local: LocalId,
        source: Option<Source>,
    ) -> BlockId {
        if self.mode != CompilationMode::Debug {
            return block;
        }

        let usize_ty = self
            .typecheck
            .interner
            .intern(Type::Builtin(zeen_ast::types::BuiltinType::usize));

        let len_operand = match self.typecheck.interner.get(obj_ty).clone() {
            Type::Array { len: Some(len), .. } => {
                Operand::Constant(ConstValue::Int(len as i128), None)
            }
            Type::Slice { .. } => {
                let mut len_place = obj_place.clone();
                len_place.projection.push(PlaceElem::Field(SLICE_LEN_FIELD));
                self.place_to_operand(len_place, usize_ty, None)
            }
            _ => return block,
        };

        let bool_ty = self
            .typecheck
            .interner
            .intern(Type::Builtin(zeen_ast::types::BuiltinType::bool));

        let cmp_result = fb.new_temp(bool_ty);
        fb.push_stmt(
            block,
            MirStatement::Assign {
                place: Place::from_local(cmp_result),
                rvalue: Rvalue::BinaryOp {
                    op: BinaryOp::Lt,
                    lhs: Operand::Copy(Place::from_local(index_local), None),
                    rhs: len_operand.clone(),
                },
                source: source.clone(),
            },
        );

        let ok_block = fb.new_block();
        let panic_block = fb.new_block();

        fb.set_terminator(
            block,
            Terminator::SwitchInt {
                discriminant: Operand::Move(Place::from_local(cmp_result), None),
                targets: vec![(1, ok_block)],
                otherwise: panic_block,
            },
        );

        let void_ty = self.typecheck.interner.intern(Type::Void);
        let dest = fb.new_temp(void_ty);
        let panic_next = fb.new_block();

        fb.set_terminator(
            panic_block,
            Terminator::MacroCall {
                kind: HirMacroKind::Panic,
                format_chunks: Some(vec![
                    FormatChunk::Literal("index out of bounds: the len is ".into()),
                    FormatChunk::Arg(FormatSpec::Display),
                    FormatChunk::Literal(" but the index is ".into()),
                    FormatChunk::Arg(FormatSpec::Display),
                ]),
                args: vec![
                    len_operand,
                    Operand::Copy(Place::from_local(index_local), None),
                ],
                arg_types: vec![usize_ty, usize_ty],
                destination: Place::from_local(dest),
                target: None,
                source,
            },
        );
        fb.set_terminator(panic_next, Terminator::Unreachable);

        ok_block
    }

    /// Inserts a `divisor != 0` guard in front of `/` and `%` on builtin
    /// numerics when compiling in Debug mode. A zero divisor diverges into a
    /// `@panic` call. Returns the block the division must be lowered into.
    ///
    /// `rhs_local` must be a plain local holding the divisor; it is only read
    /// here by Copy, so the division rvalue can use it again.
    fn lower_div_zero_check(
        &mut self,
        fb: &mut FnBuilder,
        block: BlockId,
        rhs_local: LocalId,
        rhs_ty: TypeId,
        source: Option<Source>,
    ) -> BlockId {
        if self.mode != CompilationMode::Debug {
            return block;
        }

        let bool_ty = self
            .typecheck
            .interner
            .intern(Type::Builtin(zeen_ast::types::BuiltinType::bool));

        let zero = Operand::Constant(ConstValue::Int(0), None);

        let cmp_result = fb.new_temp(bool_ty);
        fb.push_stmt(
            block,
            MirStatement::Assign {
                place: Place::from_local(cmp_result),
                rvalue: Rvalue::BinaryOp {
                    op: BinaryOp::Ne,
                    lhs: Operand::Copy(Place::from_local(rhs_local), None),
                    rhs: zero,
                },
                source: source.clone(),
            },
        );

        let ok_block = fb.new_block();
        let panic_block = fb.new_block();

        fb.set_terminator(
            block,
            Terminator::SwitchInt {
                discriminant: Operand::Move(Place::from_local(cmp_result), None),
                targets: vec![(1, ok_block)],
                otherwise: panic_block,
            },
        );

        let void_ty = self.typecheck.interner.intern(Type::Void);
        let dest = fb.new_temp(void_ty);
        let panic_next = fb.new_block();

        fb.set_terminator(
            panic_block,
            Terminator::MacroCall {
                kind: HirMacroKind::Panic,
                format_chunks: Some(vec![FormatChunk::Literal(
                    "attempt to divide by zero".into(),
                )]),
                args: vec![],
                arg_types: vec![],
                destination: Place::from_local(dest),
                target: None,
                source,
            },
        );
        fb.set_terminator(panic_next, Terminator::Unreachable);

        ok_block
    }

    fn lower_macro_call(
        &mut self,
        fb: &mut FnBuilder,
        kind: HirMacroKind,
        args: &[Rc<HirExpr>],
        hir_id: HirId,
        block: BlockId,
        source: Source,
    ) -> (BlockId, Operand) {
        let format_chunks = self.typecheck.format_specs.get(&hir_id).cloned();
        let specs = format_chunks.as_deref().map(arg_specs);

        let value_exprs: &[Rc<HirExpr>] = if format_chunks.is_some() {
            &args[1..]
        } else {
            args
        };

        let mut block = block;
        let mut operands = Vec::with_capacity(value_exprs.len());
        let mut arg_types = Vec::with_capacity(value_exprs.len());
        for (i, arg) in value_exprs.iter().enumerate() {
            let ty = self.expr_type(fb, arg);
            let spec = specs.as_deref().and_then(|s| s.get(i)).copied();

            // Struct operands with a `{}` / `{:?}` spec are lowered to a call
            // to their `display`/`debug` interface method; the returned
            // `[]const char` slice becomes the format argument.
            let (b, op, arg_ty) = match (spec, self.typecheck.interner.get(ty).clone()) {
                (Some(FormatSpec::Display | FormatSpec::Debug), Type::Struct { .. }) => {
                    let iface = match spec {
                        Some(FormatSpec::Display) => ("Display", "display"),
                        _ => ("Debug", "debug"),
                    };
                    self.lower_display_format_arg(fb, arg, ty, iface, block)
                }
                _ => {
                    let (b, op) = self.lower_expr_to_operand(fb, arg, block);
                    (b, op, ty)
                }
            };

            block = b;
            operands.push(op);
            arg_types.push(arg_ty);
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
                arg_types,
                destination: Place::from_local(dest),
                target: if is_diverging { None } else { Some(next) },
                source: Some(source),
            },
        );

        if is_diverging {
            fb.set_terminator(next, Terminator::Unreachable);
            (next, Operand::Constant(ConstValue::Void, None))
        } else {
            (
                next,
                self.place_to_operand(Place::from_local(dest), result_ty, None),
            )
        }
    }

    /// Lowers a `Display`/`Debug` format argument into a call to the struct's
    /// interface method. The method is resolved from the concrete struct type
    /// and monomorphized like any other method call; the returned
    /// `[]const char` slice is passed on to the format macro.
    fn lower_display_format_arg(
        &mut self,
        fb: &mut FnBuilder,
        arg: &HirExpr,
        obj_ty: TypeId,
        iface: (&str, &str),
        block: BlockId,
    ) -> (BlockId, Operand, TypeId) {
        let (iface_name, method_name) = iface;
        let Type::Struct {
            def_id: struct_def,
            generic_args,
        } = self.typecheck.interner.get(obj_ty).clone()
        else {
            return {
                let (b, op) = self.lower_expr_to_operand(fb, arg, block);
                (b, op, obj_ty)
            };
        };

        // The checker records which implementation it picked for this
        // argument; fall back to resolving here for unrecorded cases.
        let method_def = self
            .typecheck
            .format_arg_resolutions
            .get(&arg.id)
            .copied()
            .or_else(|| {
                self.resolve_interface_method(struct_def, iface_name, method_name, &generic_args)
            });

        let Some(method_def) = method_def else {
            return {
                let (b, op) = self.lower_expr_to_operand(fb, arg, block);
                (b, op, obj_ty)
            };
        };

        let (block, place) = self.lower_expr_to_place_or_temp(fb, arg, block);
        let (block, self_operand) = self.lower_place_receiver_operand(
            fb,
            place,
            obj_ty,
            method_def,
            Some(arg.source.clone()),
            block,
        );

        let mono_args = self.substitute_generic_args(fb, &generic_args);
        let owner = self.typecheck.method_owner.get(&method_def).copied();
        let mir_fn_id = self.monomorphize_fn(method_def, mono_args, owner, &[]);

        let ret_ty = match self.typecheck.def_types.get(&method_def) {
            Some(ty) => match self.typecheck.interner.get(*ty).clone() {
                Type::Fn { ret, .. } => self.substitute_fn_type(fb, ret),
                _ => self.typecheck.interner.intern(Type::Void),
            },
            None => self.typecheck.interner.intern(Type::Void),
        };

        let dest = fb.new_temp(ret_ty);
        let next = fb.new_block();

        fb.set_terminator(
            block,
            Terminator::Call {
                func: CallTarget::Direct(mir_fn_id),
                args: vec![self_operand],
                destination: Place::from_local(dest),
                target: Some(next),
                source: None,
            },
        );

        let op = self.place_to_operand(Place::from_local(dest), ret_ty, None);
        (next, op, ret_ty)
    }

    /// Resolves the `DefId` of the method with `method_name` that implements
    /// `iface_name` for `struct_def`, mirroring the typechecker's
    /// interface-call resolution. A concrete specialization for the given
    /// `generic_args` wins over the generic implementation.
    fn resolve_interface_method(
        &self,
        struct_def: DefId,
        iface_name: &str,
        method_name: &str,
        generic_args: &[TypeId],
    ) -> Option<DefId> {
        let iface_def = self
            .resolution
            .defs
            .iter()
            .find(|(_, info)| {
                matches!(info.kind, DefKind::Interface)
                    && self.rodeo.borrow().resolve(&info.name) == iface_name
            })
            .map(|(def, _)| *def)?;

        if let Some(entries) = self.typecheck.impl_registry.get(&(struct_def, iface_def)) {
            let entry = entries
                .iter()
                .find(|e| e.is_specialized && e.object_args.as_slice() == generic_args)
                .or_else(|| {
                    entries
                        .iter()
                        .find(|e| !e.is_specialized && !e.generic_bounds.is_empty())
                })
                .or_else(|| entries.iter().find(|e| !e.is_specialized));

            if let Some(entry) = entry {
                return entry.methods.iter().copied().find(|&def| {
                    let Some(info) = self.resolution.defs.get(&def) else {
                        return false;
                    };
                    self.rodeo.borrow().resolve(&info.name) == method_name
                });
            }
        }

        let methods = self.resolution.impls.get(&(struct_def, iface_def))?;
        methods.iter().copied().find(|&def| {
            let Some(info) = self.resolution.defs.get(&def) else {
                return false;
            };
            self.rodeo.borrow().resolve(&info.name) == method_name
        })
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
                arg_types: Vec::new(),
                destination: Place::from_local(dest),
                target: None,
                source: None,
            },
        );

        fb.set_terminator(next, Terminator::Unreachable);
        (next, Operand::Constant(ConstValue::Void, None))
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
        extra_args: &[(Operand, TypeId)],
        op_res: &OperatorResolution,
        block: BlockId,
        result_ty: TypeId,
    ) -> (BlockId, Operand) {
        let (block, self_operand) =
            self.lower_receiver_operand(fb, reciever_expr, op_res.method_def, block);

        self.lower_operator_method_call_from_operands(
            fb,
            op_res,
            self_operand,
            extra_args.to_vec(),
            block,
            result_ty,
        )
    }

    fn lower_operator_method_call_from_operands(
        &mut self,
        fb: &mut FnBuilder,
        op_res: &OperatorResolution,
        self_operand: Operand,
        extra_args: Vec<(Operand, TypeId)>,
        block: BlockId,
        result_ty: TypeId,
    ) -> (BlockId, Operand) {
        // The method's non-self parameters drive whether each operator RHS is
        // passed by value or by address (`Eq.eq` takes `other: *const Self`).
        let arg_params: Vec<TypeId> = self
            .typecheck
            .def_types
            .get(&op_res.method_def)
            .and_then(|ty| match self.typecheck.interner.get(*ty) {
                Type::Fn { params, .. } if params.len() > 1 => Some(params[1..].to_vec()),
                _ => None,
            })
            .unwrap_or_default();

        let (block, extra_args) =
            self.lower_operator_args_from_operands(fb, extra_args, &arg_params, block);

        let mono_args = self.substitute_generic_args(fb, &op_res.generic_args);
        let mir_fn_id = self.monomorphize_fn(
            op_res.method_def,
            mono_args,
            self.typecheck.method_owner.get(&op_res.method_def).copied(),
            &[],
        );

        let mut args = vec![self_operand];
        args.extend(extra_args);

        let dest = fb.new_temp(result_ty);
        let next = fb.new_block();

        fb.set_terminator(
            block,
            Terminator::Call {
                func: CallTarget::Direct(mir_fn_id),
                args,
                destination: Place::from_local(dest),
                target: Some(next),
                source: None,
            },
        );

        (
            next,
            self.place_to_operand(Place::from_local(dest), result_ty, None),
        )
    }

    fn lower_operator_args_from_operands(
        &mut self,
        fb: &mut FnBuilder,
        extra_args: Vec<(Operand, TypeId)>,
        arg_params: &[TypeId],
        block: BlockId,
    ) -> (BlockId, Vec<Operand>) {
        let mut out = Vec::with_capacity(extra_args.len());
        let mut block = block;

        for (operand, arg_ty) in extra_args {
            let expected = arg_params.get(out.len()).copied();

            let param_ty = expected.map(|t| self.typecheck.interner.get(t).clone());

            let operand = match param_ty {
                Some(Type::Pointer { is_const, .. })
                    if !matches!(
                        self.typecheck.interner.get(arg_ty),
                        Type::Pointer { .. } | Type::ManyPointer { .. }
                    ) =>
                {
                    let ptr_ty = self.typecheck.interner.intern(Type::Pointer {
                        inner: arg_ty,
                        is_const,
                    });
                    let local = self.operand_to_local(fb, operand, arg_ty, block);
                    let temp = fb.new_temp(ptr_ty);

                    fb.push_stmt(
                        block,
                        MirStatement::Assign {
                            place: Place::from_local(temp),
                            rvalue: Rvalue::Ref {
                                place: Place::from_local(local),
                                is_const,
                            },
                            source: None,
                        },
                    );
                    Operand::Move(Place::from_local(temp), None)
                }
                _ => operand,
            };

            out.push(operand);
        }

        (block, out)
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
                (block, Operand::Constant(ConstValue::Void, None))
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
                // A `Fn`/`FnOnce`-bound recorded on the statement itself is
                // not a storage type: the variable holds the concrete closure
                // type resolved into its def during finalization.
                let ty = if matches!(
                    self.typecheck.interner.get(ty),
                    Type::FatFn {
                        body: FatFnBody::Bound,
                        ..
                    }
                ) {
                    self.typecheck.def_types.get(def_id).copied().unwrap_or(ty)
                } else {
                    ty
                };

                // `let _ = expr;` discards the value: evaluate the expression
                // for its side effects but don't allocate a storage local.
                // A non-constant operand rooted at a real user variable is
                // still consumed (reads/moves keep mattering), so it gets a
                // `Discard` statement; temporaries and literals need none.
                if self.rodeo.borrow().resolve(name) == "_" {
                    return match value {
                        Some(v) => {
                            let (block, operand) = self.lower_expr_to_operand(fb, v, block);
                            let is_user = match &operand {
                                Operand::Copy(place, _) | Operand::Move(place, _) => {
                                    fb.func.local(place.local).kind != LocalKind::Temporary
                                }
                                Operand::Constant(_, _) => false,
                            };
                            if is_user {
                                fb.push_stmt(block, MirStatement::Discard(operand));
                            }
                            block
                        }
                        None => block,
                    };
                }

                let local = fb.new_local(
                    ty,
                    LocalKind::UserVariable,
                    Mutability::Mut,
                    Some(*name),
                    Some(stmt.source.clone()),
                );
                fb.locals_by_def.insert(*def_id, local);
                fb.push_stmt(block, MirStatement::StorageLive(local));
                if let Some(scope) = fb.scope_stack.last_mut() {
                    scope.push(local);
                }

                if let Some(v) = value {
                    let (block, operand) = self.lower_expr_to_operand(fb, v, block);

                    fb.push_stmt(
                        block,
                        MirStatement::Assign {
                            place: Place::from_local(local),
                            rvalue: Rvalue::Use(operand),
                            source: Some(stmt.source.clone()),
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
                        source: Some(stmt.source.clone()),
                    },
                );
                block
            }

            HirStmtKind::CompoundAssign { object, value, op } => {
                let (block, place) = self.lower_expr_to_place(fb, object, block);
                let place_ty = self.place_type(fb, &place);

                // A deref target (`*ref += v`) is not a compound-assign on a
                // struct interface: `operator_resolutions[object.id]` holds the
                // `DerefPtr` resolution that produced the place, so routing it
                // through the interface path would call `deref_ptr` with the
                // RHS as a bogus argument. Fall through to the builtin flow,
                // which reads the pointee, applies the binary op and writes it
                // back through the same deref place.
                let is_deref_target = matches!(place.projection.last(), Some(PlaceElem::Deref));

                if !is_deref_target
                    && let Some(op_res) =
                        self.typecheck.operator_resolutions.get(&object.id).cloned()
                {
                    let (block, rhs_operand) = self.lower_expr_to_operand(fb, value, block);
                    let rhs_ty = self.expr_type(fb, value);
                    let (block, self_operand) = self.lower_place_receiver_operand(
                        fb,
                        place.clone(),
                        place_ty,
                        op_res.method_def,
                        Some(stmt.source.clone()),
                        block,
                    );
                    let (block, result_operand) = self.lower_operator_method_call_from_operands(
                        fb,
                        &op_res,
                        self_operand,
                        vec![(rhs_operand, rhs_ty)],
                        block,
                        place_ty,
                    );

                    fb.push_stmt(
                        block,
                        MirStatement::Assign {
                            place,
                            rvalue: Rvalue::Use(result_operand),
                            source: Some(stmt.source.clone()),
                        },
                    );

                    return block;
                }

                let lhs_operand =
                    self.place_to_operand(place.clone(), place_ty, Some(stmt.source.clone()));

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
                        source: Some(stmt.source.clone()),
                    },
                );
                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place,
                        rvalue: Rvalue::Use(Operand::Move(Place::from_local(temp), None)),
                        source: Some(stmt.source.clone()),
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
                    let ty = self.expr_type(fb, iterator);
                    (block, ty)
                };

                match self.typecheck.interner.get(iter_ty).clone() {
                    Type::Builtin(b) if builtin_is_integer(b) => {
                        self.lower_for_range(fb, def_id, iterator, body, block)
                    }

                    Type::IntLiteral => self.lower_for_range(fb, def_id, iterator, body, block),

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
                        fb.set_terminator(
                            block,
                            Terminator::Return(self.normalize_return_operand(fb, op)),
                        );
                        return block;
                    }
                    None => Operand::Constant(ConstValue::Void, None),
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
                // A statement-position expression whose value is discarded
                // (`foo();`): warn unless the value is void/never (`@println`,
                // `@panic`, ...).
                if !matches!(
                    self.typecheck
                        .expr_types
                        .get(&expr.id)
                        .map(|ty| self.typecheck.interner.get(*ty)),
                    Some(Type::Void | Type::Never)
                ) {
                    let what = match &expr.kind {
                        HirExprKind::Call { callee, .. } => match &callee.kind {
                            HirExprKind::VarRef(def_id) => {
                                let name = self
                                    .resolution
                                    .defs
                                    .get(def_id)
                                    .map(|info| self.rodeo.borrow().resolve(&info.name).to_string())
                                    .unwrap_or_default();
                                SmolStr::from(format!("function call `{name}`"))
                            }
                            _ => SmolStr::from("expression"),
                        },
                        _ => SmolStr::from("expression"),
                    };
                    self.warnings.push(MirWarning::UnusedExpressionResult {
                        what,
                        src: expr.source.src(),
                        span: expr.source.span,
                    });
                }

                let (block, _operand) = self.lower_expr_to_operand(fb, expr, block);
                block
            }

            // Nested function declarations produce no runtime code at their
            // site; the function is lowered on demand when it is called.
            HirStmtKind::FnDecl(_) => block,

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

        // The counter must share the loop variable's type (which always
        // matches the count's type): a `usize`-typed counter compared against
        // an `i32` bound (or copied into an `i32` loop var) would emit
        // mismatched IR and corrupt the stack slot via an oversized store.
        let loop_var_ty = self
            .typecheck
            .def_types
            .get(def_id)
            .copied()
            .unwrap_or(usize_ty);
        let counter = fb.new_local(
            loop_var_ty,
            LocalKind::Temporary,
            Mutability::Mut,
            None,
            None,
        );
        fb.push_stmt(
            block,
            MirStatement::Assign {
                place: Place::from_local(counter),
                rvalue: Rvalue::Use(Operand::Constant(ConstValue::Int(0), None)),
                source: None,
            },
        );

        let header = fb.new_block();
        fb.set_terminator(block, Terminator::Goto(header));

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
                rvalue: Rvalue::Use(Operand::Copy(Place::from_local(counter), None)),
                source: None,
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
                    lhs: Operand::Copy(Place::from_local(counter), None),
                    rhs: count_operand,
                },
                source: None,
            },
        );

        let body_bb = fb.new_block();
        let exit_bb = fb.new_block();
        let continue_bb = fb.new_block();

        fb.set_terminator(
            header,
            Terminator::SwitchInt {
                discriminant: Operand::Move(Place::from_local(cmp_result), None),
                targets: vec![(1, body_bb)],
                otherwise: exit_bb,
            },
        );

        // `continue` must run the increment before re-checking the condition,
        // so it targets the dedicated increment block instead of the header.
        fb.loop_stack.push(LoopTargets {
            break_target: exit_bb,
            continue_target: continue_bb,
        });
        let body_end = self.lower_stmt_as_block_value(fb, body, body_bb).0;
        fb.loop_stack.pop();

        let incremented = fb.new_temp(loop_var_ty);
        fb.push_stmt(
            continue_bb,
            MirStatement::Assign {
                place: Place::from_local(incremented),
                rvalue: Rvalue::BinaryOp {
                    op: BinaryOp::Add,
                    lhs: Operand::Copy(Place::from_local(counter), None),
                    rhs: Operand::Constant(ConstValue::Int(1), None),
                },
                source: None,
            },
        );
        fb.push_stmt(
            continue_bb,
            MirStatement::Assign {
                place: Place::from_local(counter),
                rvalue: Rvalue::Use(Operand::Move(Place::from_local(incremented), None)),
                source: None,
            },
        );
        fb.set_terminator(continue_bb, Terminator::Goto(header));
        fb.join_if_open(body_end, continue_bb);

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
        let (block, iter_place) = self.lower_expr_to_place_or_temp(fb, iterator, block);

        let usize_ty = self
            .typecheck
            .interner
            .intern(Type::Builtin(zeen_ast::types::BuiltinType::usize));

        let (len_operand, elem_ty) = match self.typecheck.interner.get(iter_ty).clone() {
            Type::Array { element, len } => {
                let len_val = len.expect("unknown array length (must be comptime known)");

                (
                    Operand::Constant(ConstValue::Int(len_val as i128), None),
                    element,
                )
            }
            Type::Slice { element, .. } => {
                let mut len_place = iter_place.clone();
                len_place.projection.push(PlaceElem::Field(SLICE_LEN_FIELD));

                let len_local = fb.new_temp(usize_ty);

                fb.push_stmt(
                    block,
                    MirStatement::Assign {
                        place: Place::from_local(len_local),
                        rvalue: Rvalue::Use(Operand::Copy(len_place, None)),
                        source: None,
                    },
                );

                (Operand::Move(Place::from_local(len_local), None), element)
            }

            _err_type => panic!("non-iterable type: {:?}", _err_type),
        };

        let counter = fb.new_local(usize_ty, LocalKind::Temporary, Mutability::Mut, None, None);
        fb.push_stmt(
            block,
            MirStatement::Assign {
                place: Place::from_local(counter),
                rvalue: Rvalue::Use(Operand::Constant(ConstValue::Int(0), None)),
                source: None,
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
                    lhs: Operand::Copy(Place::from_local(counter), None),
                    rhs: len_operand,
                },
                source: None,
            },
        );

        let body_bb = fb.new_block();
        let exit_bb = fb.new_block();
        let continue_bb = fb.new_block();

        fb.set_terminator(
            header,
            Terminator::SwitchInt {
                discriminant: Operand::Move(Place::from_local(cmp_result), None),
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

                ptr_place.projection.push(PlaceElem::Field(SLICE_PTR_FIELD));
                ptr_place.index(counter)
            }
            _ => unreachable!(),
        };

        let elem_operand =
            self.place_to_operand(elem_place, elem_ty, Some(iterator.source.clone()));

        fb.push_stmt(
            body_bb,
            MirStatement::Assign {
                place: Place::from_local(loop_var),
                rvalue: Rvalue::Use(elem_operand),
                source: None,
            },
        );

        fb.loop_stack.push(LoopTargets {
            break_target: exit_bb,
            continue_target: continue_bb,
        });

        let body_end = self.lower_stmt_as_block_value(fb, body, body_bb).0;

        fb.loop_stack.pop();

        let incremented = fb.new_temp(usize_ty);

        fb.push_stmt(
            continue_bb,
            MirStatement::Assign {
                place: Place::from_local(incremented),
                rvalue: Rvalue::BinaryOp {
                    op: BinaryOp::Add,
                    lhs: Operand::Copy(Place::from_local(counter), None),
                    rhs: Operand::Constant(ConstValue::Int(1), None),
                },
                source: None,
            },
        );

        fb.push_stmt(
            continue_bb,
            MirStatement::Assign {
                place: Place::from_local(counter),
                rvalue: Rvalue::Use(Operand::Move(Place::from_local(incremented), None)),
                source: None,
            },
        );

        fb.set_terminator(continue_bb, Terminator::Goto(header));
        fb.join_if_open(body_end, continue_bb);

        exit_bb
    }
}

impl<'ctx> MirLowering<'ctx> {
    fn monomorphize_fn(
        &mut self,
        def_id: DefId,
        generic_args: Vec<TypeId>,
        owner_struct: Option<DefId>,
        fat_args: &[TypeId],
    ) -> MirFunctionId {
        let hir_fn = self.hir_fns_by_def[&def_id].clone();
        let key = (def_id, generic_args.clone(), fat_args.to_vec());

        if let Some(&existing) = self.mono_cache.cache.get(&key) {
            return existing;
        }

        let id = self.mono_cache.fresh_id();
        self.mono_cache.cache.insert(key, id);

        let display_name = self.compute_fn_readable_name(&hir_fn, &generic_args, owner_struct);
        let display_name = if fat_args.is_empty() {
            display_name
        } else {
            let fat_names: Vec<String> = fat_args
                .iter()
                .map(|&t| self.display_type_name(t))
                .collect();
            format!("{display_name}~f{}", fat_names.join("_"))
        };

        if hir_fn.is_extern {
            self.set_function_name(id, display_name.clone());
            self.program.extern_exports.insert(id, display_name.clone());
        } else {
            self.set_function_name(id, display_name.clone());
        }

        self.fn_stack.push(FnContext {
            def_id,
            readable_name: display_name,
        });

        let mir_func = self.lower_fn_body(def_id, &hir_fn, &generic_args, owner_struct, fat_args);
        for local in &mir_func.locals {
            if matches!(self.typecheck.interner.get(local.ty), Type::Slice { .. }) {
                self.register_slice_layout(local.ty);
            }
        }
        self.program.functions.insert(id, mir_func);

        self.fn_stack.pop();

        id
    }

    /// Computes the readable MIR name of a function. Methods are
    /// `Struct.method`, nested functions are `<parent>-><name>`, everything
    /// else is the plain name (with generic args where present).
    fn compute_fn_readable_name(
        &self,
        hir_fn: &HirFn,
        generic_args: &[TypeId],
        owner_struct: Option<DefId>,
    ) -> String {
        let interner = self.rodeo.borrow();
        let base_name = interner.resolve(&hir_fn.name.0).to_string();
        drop(interner);

        let base = if generic_args.is_empty() {
            base_name.clone()
        } else {
            let arg_names: Vec<String> = generic_args
                .iter()
                .map(|&t| self.display_type_name(t))
                .collect();
            format!("{}[{}]", base_name, arg_names.join(", "))
        };

        if let Some(struct_def) = owner_struct {
            let struct_name = self
                .rodeo
                .borrow()
                .resolve(&self.resolution.defs[&struct_def].name)
                .to_string();

            let struct_generics = self
                .typecheck
                .struct_generics
                .get(&struct_def)
                .cloned()
                .unwrap_or_default();

            let struct_part = if struct_generics.is_empty() {
                struct_name
            } else {
                let arg_names: Vec<String> = generic_args
                    .iter()
                    .take(struct_generics.len())
                    .map(|&t| self.display_type_name(t))
                    .collect();
                format!("{}[{}]", struct_name, arg_names.join(", "))
            };

            return format!("{}.{}", struct_part, base_name);
        }

        if let Some(parent_def) = hir_fn.parent_fn
            && let Some(parent_name) = self
                .fn_stack
                .iter()
                .rev()
                .find(|ctx| ctx.def_id == parent_def)
                .map(|ctx| ctx.readable_name.clone())
        {
            return format!("{}->{}", parent_name, base);
        }

        base
    }

    /// Replaces a void-typed place operand with a plain `void` constant so
    /// codegen never tries to load an un-allocated void temporary.
    fn normalize_return_operand(&mut self, fb: &FnBuilder, operand: Operand) -> Operand {
        match &operand {
            Operand::Copy(place, _) | Operand::Move(place, _) => {
                let ty = fb.func.local(place.local).ty;
                if matches!(self.typecheck.interner.get(ty), Type::Void) {
                    Operand::Constant(ConstValue::Void, None)
                } else {
                    operand
                }
            }
            _ => operand,
        }
    }

    fn lower_fn_body(
        &mut self,
        def_id: DefId,
        hir_fn: &HirFn,
        generic_args: &[TypeId],
        owner_struct: Option<DefId>,
        fat_args: &[TypeId],
    ) -> MirFunction {
        let generic_defs: Vec<DefId> = hir_fn.generics.iter().map(|g| g.def_id).collect();
        let bindings: HashMap<DefId, TypeId> = if !generic_defs.is_empty() {
            generic_defs
                .iter()
                .copied()
                .zip(generic_args.iter().copied())
                .collect()
        } else if let Some(owner) = owner_struct {
            let struct_generics = self
                .typecheck
                .struct_generics
                .get(&owner)
                .cloned()
                .unwrap_or_default();
            let mut bindings: HashMap<DefId, TypeId> = struct_generics
                .iter()
                .copied()
                .zip(generic_args.iter().copied())
                .collect();

            // An implement block's methods may be written with the block's
            // own generic parameters (`implement[T] Deref : Holder[T]`):
            // substitute them through the struct's generic slots.
            for entries in self.typecheck.impl_registry.values() {
                for entry in entries {
                    if !entry.methods.contains(&def_id) {
                        continue;
                    }

                    for (arg, &struct_g) in entry.object_args.iter().zip(struct_generics.iter()) {
                        if let Type::GenericParam(imp_g) = self.typecheck.interner.get(*arg)
                            && let Some(&concrete) = bindings.get(&struct_g)
                        {
                            bindings.insert(*imp_g, concrete);
                        }
                    }
                }
            }

            bindings
        } else {
            HashMap::new()
        };

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

        // A `Fn`/`FnOnce` return annotation is a bound, not a storage type:
        // the function actually returns the concrete closure type derived
        // from its body during the typecheck finalization.
        let ret_ty = if matches!(
            self.typecheck.interner.get(ret_ty),
            Type::FatFn {
                body: FatFnBody::Bound,
                ..
            }
        ) {
            self.typecheck
                .fn_return_fats
                .get(&def_id)
                .copied()
                .unwrap_or(ret_ty)
        } else {
            ret_ty
        };

        let entry = BlockId(0);
        let mut fb = FnBuilder::new(def_id, generic_args.to_vec(), entry, ret_ty, HashMap::new());
        fb.new_block();
        let mut fat_bound_count = 0usize;

        // Closure functions receive their captured environment as a leading
        // `*const` pointer parameter. Captured variables are read through it
        // (`env->$env0`, `env->$env1`, ...), so captured binds keep their
        // enclosing-frame `DefId`s and resolve to projections of this pointer.
        let captures = self
            .resolution
            .closure_captures
            .get(&def_id)
            .cloned()
            .unwrap_or_default();

        if !captures.is_empty() {
            let env_def = zeen_types::closure_struct_def(def_id);
            let env_ty = self.typecheck.interner.intern(Type::Struct {
                def_id: env_def,
                generic_args: Vec::new(),
            });
            let env_ptr_ty = self.typecheck.interner.intern(Type::Pointer {
                inner: env_ty,
                is_const: true,
            });

            let env_param = fb.new_local(env_ptr_ty, LocalKind::Param, Mutability::Mut, None, None);
            fb.func.params.push(env_param);
            let env_place = Place::from_local(env_param);

            for (index, captured) in captures.iter().enumerate() {
                let field_place = env_place
                    .clone()
                    .deref()
                    .field(closure_field_def(def_id, index));
                fb.captured_places.insert(*captured, field_place);
            }
        }

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

            // A `Fn`/`FnOnce`-annotated parameter is erased in the signature;
            // this monomorphized copy stores the concrete closure type of the
            // actual call-site argument.
            let param_is_fat_bound = matches!(
                self.typecheck.interner.get(concrete_ty),
                Type::FatFn {
                    body: FatFnBody::Bound,
                    ..
                }
            );
            let storage_ty = if param_is_fat_bound {
                let idx = fat_bound_count;
                fat_bound_count += 1;
                fat_args.get(idx).copied().unwrap_or(concrete_ty)
            } else {
                concrete_ty
            };

            if param_is_fat_bound {
                fb.fat_bindings.push((param_def, storage_ty));
            }

            let local = fb.new_local(
                storage_ty,
                LocalKind::Param,
                Mutability::Mut,
                param.name,
                Some(param.ty.source.clone()),
            );

            fb.func.params.push(local);
            fb.locals_by_def.insert(param_def, local);
        }

        fb.bindings = bindings;

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
                                let operand = self.normalize_return_operand(&fb, operand);
                                fb.set_terminator(block, Terminator::Return(operand));
                            };
                            block
                        }

                        None => {
                            if matches!(fb.func.block(cur).terminator, Terminator::Unreachable) {
                                fb.set_terminator(
                                    cur,
                                    Terminator::Return(Operand::Constant(ConstValue::Void, None)),
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
                            Terminator::Return(Operand::Constant(ConstValue::Void, None)),
                        );
                    }
                    cur
                }
            }

            _ => {
                let cur = self.lower_stmt(&mut fb, body, entry);
                if matches!(fb.func.block(cur).terminator, Terminator::Unreachable) {
                    fb.set_terminator(
                        cur,
                        Terminator::Return(Operand::Constant(ConstValue::Void, None)),
                    );
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
        let callee_ty = self.expr_type(fb, callee);

        // Fat closure/coerced function: dispatch through the `{ $fn, $env }`
        // envelope with the uniform env-first ABI.
        if matches!(self.typecheck.interner.get(callee_ty), Type::FatFn { .. }) {
            return self.lower_fat_call(fb, callee, args, block, ret_ty);
        }

        let (mut block, callee_operand) = self.lower_expr_to_operand(fb, callee, block);

        let mut arg_operands = Vec::with_capacity(args.len());
        for arg in args.iter() {
            let (b, op) = self.lower_expr_to_operand(fb, arg, block);
            block = b;
            arg_operands.push(op);
        }

        self.emit_call(
            fb,
            CallTarget::Indirect(callee_operand),
            arg_operands,
            block,
            ret_ty,
            callee,
        )
    }

    /// Calls a fat closure value. The called function is known statically
    /// from the value's type: a `Closure` body dispatches directly to its
    /// target with a pointer to the value (which *is* the environment) as the
    /// leading env-first argument; a `Pointer` body calls the fn pointer it
    /// holds with the plain ABI. An `FnOnce` value is consumed by the call —
    /// the whole value moves into a slot so dataflow rejects a second call —
    /// and its captured values are torn down right after the call returns.
    fn lower_fat_call(
        &mut self,
        fb: &mut FnBuilder,
        callee: &HirExpr,
        args: &[Rc<HirExpr>],
        block: BlockId,
        ret_ty: TypeId,
    ) -> (BlockId, Operand) {
        let callee_ty = self.expr_type(fb, callee);
        let (once, body) = match self.typecheck.interner.get(callee_ty).clone() {
            Type::FatFn { once, body, .. } => (once, body),
            _ => panic!("fat call requires a fat callee"),
        };
        let diverging = matches!(self.typecheck.interner.get(ret_ty), Type::Never);

        let (mut block, closure_operand) = self.lower_expr_to_operand(fb, callee, block);

        let closure_place = match &closure_operand {
            Operand::Copy(place, _) | Operand::Move(place, _) => place.clone(),
            Operand::Constant(..) => panic!("fat closure value must live in a place"),
        };

        // Consuming call: move the whole closure value into a dedicated slot,
        // so dataflow marks the value used-up.
        let closure_place = if once {
            let slot = fb.new_temp(callee_ty);
            fb.push_stmt(
                block,
                MirStatement::Assign {
                    place: Place::from_local(slot),
                    rvalue: Rvalue::Use(closure_operand),
                    source: Some(callee.source.clone()),
                },
            );
            Place::from_local(slot)
        } else {
            closure_place
        };

        let mut arg_operands = Vec::with_capacity(args.len() + 1);
        let call_target: CallTarget = match body {
            FatFnBody::Closure { env, target } => {
                // Capturing closure bodies use the env-first ABI: pass
                // `&value` (which *is* the environment) as the leading
                // argument. Zero-capture closures have an empty env, so
                // their bodies use the plain ABI.
                let has_captures = !self.env_struct_fields(env).is_empty();
                if has_captures {
                    let env_ptr_ty = self.typecheck.interner.intern(Type::Pointer {
                        inner: callee_ty,
                        is_const: true,
                    });
                    let env_temp = fb.new_temp(env_ptr_ty);
                    fb.push_stmt(
                        block,
                        MirStatement::Assign {
                            place: Place::from_local(env_temp),
                            rvalue: Rvalue::Ref {
                                place: closure_place.clone(),
                                is_const: true,
                            },
                            source: None,
                        },
                    );
                    arg_operands.push(Operand::Copy(Place::from_local(env_temp), None));
                }
                CallTarget::Direct(self.monomorphize_fn(target, Vec::new(), None, &[]))
            }
            FatFnBody::Pointer { .. } => {
                // A wrapped fn pointer: call it directly with the plain ABI.
                let fn_ptr = Operand::Copy(closure_place.clone().field(CLOSURE_FAT_FN_FIELD), None);
                CallTarget::Indirect(fn_ptr)
            }
            FatFnBody::Bound => unreachable!("an erased bound is not a storage type"),
        };

        for arg in args.iter() {
            let (b, op) = self.lower_expr_to_operand(fb, arg, block);
            block = b;
            arg_operands.push(op);
        }

        let (block, result) = self.emit_call(fb, call_target, arg_operands, block, ret_ty, callee);

        // A consuming call ends the value's life here: tear its captured
        // values down right after the call returns. Diverging calls never
        // return, so there is nothing to release on that path.
        if !once || diverging || !matches!(body, FatFnBody::Closure { .. }) {
            return (block, result);
        }

        let drop_id = self.fat_drop_function(callee_ty);
        let void_ty = self.typecheck.interner.intern(Type::Void);
        let sink = fb.new_temp(void_ty);
        let next = fb.new_block();
        fb.set_terminator(
            block,
            Terminator::Call {
                func: CallTarget::Direct(drop_id),
                args: vec![Operand::Copy(closure_place, None)],
                destination: Place::from_local(sink),
                target: Some(next),
                source: Some(callee.source.clone()),
            },
        );

        (next, result)
    }

    fn emit_call(
        &mut self,
        fb: &mut FnBuilder,
        target: CallTarget,
        arg_operands: Vec<Operand>,
        block: BlockId,
        ret_ty: TypeId,
        source_expr: &HirExpr,
    ) -> (BlockId, Operand) {
        let dest_local = fb.new_temp(ret_ty);
        let dest_place = Place::from_local(dest_local);
        let next_block = fb.new_block();

        let is_diverging = matches!(self.typecheck.interner.get(ret_ty), Type::Never);

        fb.set_terminator(
            block,
            Terminator::Call {
                func: target,
                args: arg_operands,
                destination: dest_place.clone(),
                target: if is_diverging { None } else { Some(next_block) },
                source: Some(source_expr.source.clone()),
            },
        );

        if is_diverging {
            fb.set_terminator(next_block, Terminator::Unreachable);
            (next_block, Operand::Constant(ConstValue::Void, None))
        } else {
            (
                next_block,
                self.place_to_operand(dest_place, ret_ty, Some(source_expr.source.clone())),
            )
        }
    }

    fn resolve_call_target(
        &mut self,
        fn_def: DefId,
        generic_args: Vec<TypeId>,
        hir_fn: &Rc<HirFn>,
        fat_args: &[TypeId],
    ) -> CallTarget {
        if hir_fn.is_extern && hir_fn.body.is_none() {
            let idx = self.register_extern_fn(fn_def, hir_fn);
            CallTarget::Extern(idx)
        } else {
            let mir_id = self.monomorphize_fn(
                fn_def,
                generic_args,
                self.typecheck.method_owner.get(&fn_def).copied(),
                fat_args,
            );
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
