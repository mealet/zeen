use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use bumpalo::Bump;
use lasso::{Rodeo, Spur};

use miette::NamedSource;

use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;

use zeen_ast::{
    Declaration, DeclarationKind, Expression, ExpressionKind, Statement, StatementKind,
    expressions::{BinaryOp, Literal, UnaryOp},
};

use crate::error::Error;
use crate::value::Value;

pub const STEP_LIMIT: usize = 200_000;
pub const DEPTH_LIMIT: usize = 200;

pub type EvalFrame<'arena> = Vec<FxHashMap<Spur, Value<'arena>>>;

pub type EvalResult<'arena> = Result<Value<'arena>, Unwind<'arena>>;
pub type StmtResult<'arena> = Result<Option<Value<'arena>>, Unwind<'arena>>;

#[derive(Debug)]
pub enum Unwind<'arena> {
    Return(Value<'arena>),
    Break,
    Continue,
}

pub struct ModuleScope<'arena> {
    pub(crate) src: NamedSource<Arc<String>>,
    pub(crate) consts: HashMap<Spur, ConstEntry<'arena>>,
    pub(crate) fns: HashMap<Spur, &'arena Declaration<'arena>>,
    pub(crate) structs: HashMap<Spur, &'arena Declaration<'arena>>,
    pub(crate) enums: HashMap<Spur, &'arena Declaration<'arena>>,
}

pub enum ConstEntry<'arena> {
    Pending(&'arena Expression<'arena>, miette::SourceSpan),
    Evaluating(miette::SourceSpan),
    Done(Value<'arena>),
}

pub struct CallFrame<'arena> {
    pub(crate) module: PathBuf,
    pub(crate) src: NamedSource<Arc<String>>,
    pub(crate) scopes: EvalFrame<'arena>,
    pub(crate) caller_name: Option<(Spur, miette::SourceSpan)>,
}

pub struct Interp<'arena> {
    pub(crate) arena: &'arena Bump,
    pub(crate) interner: Rc<RefCell<Rodeo>>,

    pub(crate) scopes: FxHashMap<PathBuf, ModuleScope<'arena>>,

    pub(crate) frames: Vec<CallFrame<'arena>>,

    pub(crate) default_src: Option<NamedSource<Arc<String>>>,

    pub(crate) shadow_stack: Vec<FxHashSet<Spur>>,

    pub(crate) emitted_decls: Vec<&'arena Declaration<'arena>>,

    pub(crate) steps: usize,
    pub(crate) depth: usize,

    pub(crate) errors: Vec<Error>,
}

impl<'arena> Interp<'arena> {
    pub fn new(arena: &'arena Bump, interner: Rc<RefCell<Rodeo>>) -> Self {
        Self {
            arena,
            interner,
            scopes: FxHashMap::default(),
            frames: Vec::new(),
            default_src: None,
            shadow_stack: Vec::new(),
            emitted_decls: Vec::new(),
            steps: 0,
            depth: 0,
            errors: Vec::new(),
        }
    }

    pub fn evaluate(
        &mut self,
        program: &'arena [&'arena Declaration<'arena>],
        module_of: impl Fn(&Declaration<'_>) -> PathBuf,
    ) -> Result<&'arena [&'arena Declaration<'arena>], Vec<Error>> {
        for decl in program {
            let path = module_of(decl);

            let scope = self
                .scopes
                .entry(path.clone())
                .or_insert_with(|| ModuleScope {
                    src: decl.source.src(),
                    consts: HashMap::default(),
                    fns: HashMap::default(),
                    structs: HashMap::default(),
                    enums: HashMap::default(),
                });

            match &decl.kind {
                DeclarationKind::ComptimeConst { name, value, .. } => {
                    scope
                        .consts
                        .entry(name.0)
                        .or_insert(ConstEntry::Pending(value, name.1));
                }
                DeclarationKind::ComptimeFn { name, .. } => {
                    scope.fns.entry(name.0).or_insert(decl);
                }
                DeclarationKind::StructDecl { name, .. } => {
                    scope.structs.entry(name.0).or_insert(decl);
                }
                DeclarationKind::EnumDecl { name, .. } => {
                    scope.enums.entry(name.0).or_insert(decl);
                }
                DeclarationKind::ComptimeBlock(block) => {
                    self.frames.push(CallFrame {
                        module: path.clone(),
                        src: decl.source.src(),
                        scopes: vec![FxHashMap::default()],
                        caller_name: None,
                    });
                    for stmt in block.stmts.iter() {
                        if let Err(Unwind::Break) = self.eval_statement(stmt)
                            && !self.errors.is_empty()
                        {
                            break;
                        }
                    }
                    self.frames.pop();
                }
                _ => continue,
            }
        }

        let module_ref = module_of;
        let substituted = self.substitute_program(program, &module_ref)?;

        let kept: Vec<&'arena Declaration<'arena>> = substituted
            .into_iter()
            .filter(|decl| {
                !matches!(
                    decl.kind,
                    DeclarationKind::ComptimeConst { .. }
                        | DeclarationKind::ComptimeFn { .. }
                        | DeclarationKind::ComptimeBlock(_)
                )
            })
            .collect();

        let mut result: Vec<&'arena Declaration<'arena>> = kept;
        result.extend(self.emitted_decls.iter().copied());

        Ok(self.arena.alloc_slice_copy(&result))
    }

    pub(crate) fn interned(&self, spur: &Spur) -> String {
        self.interner.borrow().resolve(spur).to_string()
    }

    pub(crate) fn current_src(&self) -> NamedSource<Arc<String>> {
        self.frames
            .last()
            .map(|frame| frame.src.clone())
            .or_else(|| self.default_src.clone())
            .unwrap_or_else(|| NamedSource::new("comptime", Arc::new(String::new())))
    }

    pub(crate) fn report(&mut self, error: Error) -> Vec<Error> {
        self.errors.push(error);
        self.errors.clone()
    }

    pub(crate) fn name_shadowed(&self, name: Spur) -> bool {
        self.shadow_stack.iter().any(|scope| scope.contains(&name))
    }

    pub(crate) fn bind_shadow(&mut self, name: Spur) {
        match self.shadow_stack.last_mut() {
            Some(scope) => {
                scope.insert(name);
            }
            None => {
                let mut scope = FxHashSet::default();
                scope.insert(name);
                self.shadow_stack.push(scope);
            }
        }
    }

    pub(crate) fn scope_src(&self, module: &PathBuf) -> NamedSource<Arc<String>> {
        self.scopes
            .get(module)
            .map(|scope| scope.src.clone())
            .unwrap_or_else(|| NamedSource::new("comptime", Arc::new(String::new())))
    }

    pub(crate) fn push_shadow(&mut self) {
        self.shadow_stack.push(FxHashSet::default());
    }

    pub(crate) fn pop_shadow(&mut self) {
        self.shadow_stack.pop();
    }

    fn top_shadow(&mut self) {
        if self.shadow_stack.is_empty() {
            self.shadow_stack.push(FxHashSet::default());
        }
    }

    pub(crate) fn is_comptime_const(&mut self, module: &PathBuf, name: Spur) -> bool {
        self.top_shadow();

        if self.name_shadowed(name) {
            return false;
        }

        match self.scopes.get(module) {
            Some(scope) => scope.consts.contains_key(&name),
            None => false,
        }
    }

    pub(crate) fn is_comptime_fn(&mut self, module: &PathBuf, name: Spur) -> bool {
        self.top_shadow();

        if self.name_shadowed(name) {
            return false;
        }

        match self.scopes.get(module) {
            Some(scope) => scope.fns.contains_key(&name),
            None => false,
        }
    }

    pub(crate) fn comptime_const_value(
        &mut self,
        module: &PathBuf,
        name: Spur,
    ) -> Option<Value<'arena>> {
        match self.eval_const(module, name) {
            Ok(value) => Some(value),
            Err(Unwind::Return(value)) => Some(value),
            Err(_) => None,
        }
    }

    pub(crate) fn eval_comptime_call(
        &mut self,
        module: &PathBuf,
        name: Spur,
        span: miette::SourceSpan,
        args: &[Value<'arena>],
    ) -> EvalResult<'arena> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            self.errors.push(Error::LimitExceeded {
                message: "comptime evaluation step limit exceeded".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        self.depth += 1;
        let overflow = self.depth > DEPTH_LIMIT;
        self.depth -= 1;

        if overflow {
            self.errors.push(Error::LimitExceeded {
                message: "comptime evaluation depth limit exceeded".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let (fn_decl, fn_src) = {
            match self.scopes.get(module) {
                Some(scope) => match scope.fns.get(&name) {
                    Some(decl) => (*decl, scope.src.clone()),
                    None => {
                        let label = self.interned(&name);
                        self.errors.push(Error::UnresolvedIdent {
                            name: SmolStr::from(label),
                            src: self.current_src(),
                            span,
                        });
                        return Err(Unwind::Break);
                    }
                },
                None => {
                    let label = self.interned(&name);
                    self.errors.push(Error::UnresolvedIdent {
                        name: SmolStr::from(label),
                        src: self.current_src(),
                        span,
                    });
                    return Err(Unwind::Break);
                }
            }
        };

        let (params, body, return_type) = match &fn_decl.kind {
            DeclarationKind::ComptimeFn {
                params,
                body,
                return_type,
                ..
            } => (*params, *body, *return_type),
            _ => {
                self.errors.push(Error::Unsupported {
                    message: "expected comptime function declaration".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let body = match body {
            Some(body) => body,
            None => {
                let label = self.interned(&name);
                self.errors.push(Error::Unsupported {
                    message: format!("comptime function `{label}` has no body").into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        if params.len() != args.len() {
            let label = self.interned(&name);
            self.errors.push(Error::TypeMismatch {
                message: format!(
                    "comptime function `{label}` expects {} argument(s), got {}",
                    params.len(),
                    args.len()
                )
                .into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let mut bindings: FxHashMap<Spur, Value<'arena>> = FxHashMap::default();
        for (param, value) in params.iter().zip(args.iter()) {
            if let Some(param_name) = param.name {
                bindings.insert(param_name, value.clone());
            }
        }

        self.frames.push(CallFrame {
            module: module.clone(),
            src: fn_src,
            scopes: vec![bindings],
            caller_name: Some((name, span)),
        });

        self.depth += 1;
        self.push_shadow();

        let empty_body_no_return = matches!(body.kind, StatementKind::Expr(expr) if matches!(&expr.kind, ExpressionKind::Block { stmts, trailing } if stmts.is_empty() && trailing.is_none()))
            && return_type.is_some();

        let result: Result<Option<Value<'arena>>, Unwind<'arena>> = match self.eval_statement(body)
        {
            Err(Unwind::Return(value)) => Ok(Some(value)),
            Ok(value) => Ok(value),
            Err(_) => Err(Unwind::Break),
        };

        if empty_body_no_return {
            self.errors.push(Error::Unsupported {
                message: format!(
                    "comptime function `{}` declared return type but body is empty",
                    self.interned(&name)
                )
                .into(),
                src: self.current_src(),
                span: body.span,
            });
        }

        self.pop_shadow();
        self.depth -= 1;
        self.frames.pop();

        match result {
            Ok(Some(value)) => Ok(value),
            Ok(None) => {
                if return_type.is_some() {
                    let label = self.interned(&name);
                    self.errors.push(Error::Unsupported {
                        message: format!(
                            "comptime function `{label}` declared return type but body produced no value"
                        )
                        .into(),
                        src: self.current_src(),
                        span,
                    });
                    Err(Unwind::Break)
                } else {
                    Ok(Value::Bool(false))
                }
            }
            Err(Unwind::Break) => Err(Unwind::Break),
            Err(_) => Err(Unwind::Break),
        }
    }

    pub fn eval_const(&mut self, module: &PathBuf, name: Spur) -> EvalResult<'arena> {
        let scope_src = self.scopes.get(module).map(|scope| scope.src.clone());

        if scope_src.is_none() {
            let label = self.interned(&name);
            self.errors.push(Error::UnresolvedIdent {
                name: SmolStr::from(label),
                src: NamedSource::new("comptime", Arc::new(String::new())),
                span: miette::SourceSpan::new(0.into(), 0),
            });
            return Err(Unwind::Break);
        }

        let mut value_expr_ref: Option<&'arena Expression<'arena>> = None;
        let mut name_span = miette::SourceSpan::new(0.into(), 0);
        let mut cyclic = false;
        let mut missing = false;

        if let Some(scope) = self.scopes.get_mut(module) {
            match scope.consts.get_mut(&name) {
                Some(ConstEntry::Done(value)) => return Ok(value.clone()),
                Some(ConstEntry::Pending(expr, span)) => {
                    value_expr_ref = Some(*expr);
                    name_span = *span;
                }
                Some(ConstEntry::Evaluating(span)) => {
                    name_span = *span;
                    cyclic = true;
                }
                None => missing = true,
            }
        }

        if cyclic {
            let label = self.interned(&name);
            self.errors.push(Error::TypeMismatch {
                message: format!("cyclic dependency for comptime constant `{label}`").into(),
                src: scope_src
                    .unwrap_or_else(|| NamedSource::new("comptime", Arc::new(String::new()))),
                span: name_span,
            });
            return Err(Unwind::Break);
        }

        if missing {
            let label = self.interned(&name);
            self.errors.push(Error::UnresolvedIdent {
                name: SmolStr::from(label),
                src: scope_src
                    .unwrap_or_else(|| NamedSource::new("comptime", Arc::new(String::new()))),
                span: miette::SourceSpan::new(0.into(), 0),
            });
            return Err(Unwind::Break);
        }

        let value_expr = value_expr_ref.expect("comptime const value expr");

        {
            let scope = self.scopes.get_mut(module).expect("scope exists");
            *scope.consts.get_mut(&name).expect("entry exists") = ConstEntry::Evaluating(name_span);
        }

        let frame_src =
            scope_src.unwrap_or_else(|| NamedSource::new("comptime", Arc::new(String::new())));

        self.frames.push(CallFrame {
            module: module.clone(),
            src: frame_src,
            scopes: vec![FxHashMap::default()],
            caller_name: None,
        });

        self.depth += 1;
        let result = self.eval_expr(value_expr);
        self.depth -= 1;

        self.frames.pop();

        let value = result?;

        {
            let scope = self.scopes.get_mut(module).expect("scope exists");
            *scope.consts.get_mut(&name).expect("entry exists") = ConstEntry::Done(value.clone());
        }

        Ok(value)
    }

    pub fn eval_statements(&mut self, stmts: &'arena [&Statement<'arena>]) -> StmtResult<'arena> {
        for (i, stmt) in stmts.iter().enumerate() {
            let is_last = stmts.iter().skip(i).count() == 1;

            match self.eval_statement(stmt) {
                Ok(Some(value)) if is_last => return Ok(Some(value)),
                Ok(Some(_)) => continue,
                Ok(None) => continue,
                Err(Unwind::Return(v)) => return Err(Unwind::Return(v)),
                Err(_) => return Err(Unwind::Break),
            }
        }

        Ok(None)
    }

    fn eval_statement(&mut self, stmt: &'arena Statement<'arena>) -> StmtResult<'arena> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            self.errors.push(Error::LimitExceeded {
                message: "comptime step limit exceeded".to_string().into(),
                src: self.current_src(),
                span: stmt.span,
            });
            return Err(Unwind::Break);
        }

        match &stmt.kind {
            StatementKind::Let {
                name,
                value,
                is_const,
                ..
            } => {
                let init = match value {
                    Some(expr) => self.eval_expr(expr),
                    None => {
                        self.errors.push(Error::TypeMismatch {
                            message: "comptime let requires an initializer".to_string().into(),
                            src: self.current_src(),
                            span: stmt.span,
                        });
                        return Err(Unwind::Break);
                    }
                }?;

                {
                    let frame = self.frames.last_mut().expect("comptime frame");
                    let scope = frame.scopes.last_mut().expect("comptime scope exists");

                    if *is_const && scope.contains_key(name) {
                        let label = self.interned(name);
                        self.errors.push(Error::TypeMismatch {
                            message: format!("cannot redefine `comptime const {}`", label).into(),
                            src: self.current_src(),
                            span: stmt.span,
                        });
                        return Err(Unwind::Break);
                    }

                    scope.insert(*name, init);
                }

                Ok(None)
            }

            StatementKind::Assign { object, value } => {
                let ExpressionKind::Ident { name, .. } = &object.kind else {
                    self.errors.push(Error::Unsupported {
                        message: "only simple names can be assigned in comptime"
                            .to_string()
                            .into(),
                        src: self.current_src(),
                        span: stmt.span,
                    });
                    return Err(Unwind::Break);
                };

                let value = self.eval_expr(value)?;

                {
                    let frame = self.frames.last_mut().expect("comptime frame");
                    if let Some(scope) = frame
                        .scopes
                        .iter_mut()
                        .rev()
                        .find(|scope| scope.contains_key(name))
                    {
                        scope.insert(*name, value);
                        return Ok(None);
                    }
                }

                let label = self.interned(name);
                self.errors.push(Error::UnresolvedIdent {
                    name: label.into(),
                    src: self.current_src(),
                    span: stmt.span,
                });
                Err(Unwind::Break)
            }

            StatementKind::CompoundAssign { object, value, op } => {
                let ExpressionKind::Ident { name, .. } = &object.kind else {
                    self.errors.push(Error::Unsupported {
                        message: "only simple names support compound assign in comptime"
                            .to_string()
                            .into(),
                        src: self.current_src(),
                        span: stmt.span,
                    });
                    return Err(Unwind::Break);
                };

                let rhs_value = self.eval_expr(value)?;
                let lhs_value = self.eval_ident_value(*name, stmt.span)?;

                let computed = self.apply_binary(*op, lhs_value, rhs_value, stmt.span)?;

                {
                    let frame = self.frames.last_mut().expect("comptime frame");
                    if let Some(scope) = frame
                        .scopes
                        .iter_mut()
                        .rev()
                        .find(|scope| scope.contains_key(name))
                    {
                        scope.insert(*name, computed);
                        return Ok(None);
                    }
                }

                let label = self.interned(name);
                self.errors.push(Error::UnresolvedIdent {
                    name: label.into(),
                    src: self.current_src(),
                    span: stmt.span,
                });
                Err(Unwind::Break)
            }

            StatementKind::Return { value } => match value {
                Some(expr) => {
                    let output = self.eval_expr(expr)?;
                    Err(Unwind::Return(output))
                }
                None => Err(Unwind::Return(Value::Bool(false))),
            },

            StatementKind::Break => Err(Unwind::Break),
            StatementKind::Continue => Err(Unwind::Continue),

            StatementKind::While { condition, block } => {
                self.depth += 1;
                if self.depth > DEPTH_LIMIT {
                    self.depth -= 1;
                    self.errors.push(Error::LimitExceeded {
                        message: "comptime recursion limit exceeded".to_string().into(),
                        src: self.current_src(),
                        span: stmt.span,
                    });
                    return Err(Unwind::Break);
                }

                loop {
                    let keep_going = match self.eval_expr(condition)? {
                        Value::Bool(v) => v,
                        other => {
                            self.errors.push(Error::TypeMismatch {
                                message: format!(
                                    "comptime `while` expects bool, got {}",
                                    other.type_name()
                                )
                                .into(),
                                src: self.current_src(),
                                span: stmt.span,
                            });
                            break;
                        }
                    };

                    if !keep_going {
                        break;
                    }

                    match self.eval_statement(block) {
                        Err(Unwind::Return(v)) => return Err(Unwind::Return(v)),
                        Err(Unwind::Break) => break,
                        Err(Unwind::Continue) => continue,
                        Ok(_) => {}
                    }
                }

                self.depth -= 1;
                Ok(None)
            }

            StatementKind::For {
                varname,
                iterator,
                block,
            } => self.eval_for_statement(varname.0, iterator, block, stmt.span),

            StatementKind::Expr(expr) => match self.eval_expr(expr) {
                Ok(value) => Ok(Some(value)),
                Err(Unwind::Return(v)) => Err(Unwind::Return(v)),
                Err(_) => Err(Unwind::Break),
            },

            StatementKind::TrailingExpr(expr) => match self.eval_expr(expr) {
                Ok(value) => Ok(Some(value)),
                Err(Unwind::Return(v)) => Err(Unwind::Return(v)),
                Err(_) => Err(Unwind::Break),
            },

            StatementKind::FnDecl(_) => {
                self.errors.push(Error::Unsupported {
                    message: "nested comptime functions are not supported"
                        .to_string()
                        .into(),
                    src: self.current_src(),
                    span: stmt.span,
                });
                Err(Unwind::Break)
            }

            StatementKind::ConditionalBlock(_) => {
                unreachable!("conditional blocks are expanded by preprocessor")
            }
        }
    }

    fn eval_for_statement(
        &mut self,
        name: Spur,
        iterator: &'arena Expression<'arena>,
        block: &'arena Statement<'arena>,
        span: miette::SourceSpan,
    ) -> StmtResult<'arena> {
        self.depth += 1;
        if self.depth > DEPTH_LIMIT {
            self.depth -= 1;
            self.errors.push(Error::LimitExceeded {
                message: "comptime recursion limit exceeded".to_string().into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let iterator_value = self.eval_expr(iterator)?;

        match iterator_value {
            Value::Int(total) if total >= 0 => {
                for index in 0..total {
                    {
                        let frame = self.frames.last_mut().expect("comptime frame");
                        let scope = frame.scopes.last_mut().expect("comptime scope exists");
                        scope.insert(name, Value::Int(index));
                    }

                    match self.eval_statement(block) {
                        Err(Unwind::Return(v)) => return Err(Unwind::Return(v)),
                        Err(Unwind::Break) => break,
                        Err(Unwind::Continue) => continue,
                        Ok(_) => {}
                    }
                }
            }

            other => {
                self.errors.push(Error::TypeMismatch {
                    message: format!(
                        "comptime `for` only supports integer ranges, got {}",
                        other.type_name()
                    )
                    .into(),
                    src: self.current_src(),
                    span,
                });
            }
        }

        self.depth -= 1;
        Ok(None)
    }

    fn eval_ident_value(&mut self, name: Spur, span: miette::SourceSpan) -> EvalResult<'arena> {
        if let Some(value) = self.local_value(name) {
            return Ok(value);
        }

        let module = self.current_module();

        if self.is_comptime_const(&module, name) {
            return self.eval_const(&module, name);
        }

        let label = self.interned(&name);
        self.errors.push(Error::UnresolvedIdent {
            name: label.into(),
            src: self.current_src(),
            span,
        });
        Err(Unwind::Break)
    }

    fn local_value(&self, name: Spur) -> Option<Value<'arena>> {
        let frame = self.frames.last()?;

        for scope in frame.scopes.iter().rev() {
            if let Some(value) = scope.get(&name) {
                return Some(value.clone());
            }
        }

        None
    }

    pub(crate) fn current_module(&self) -> PathBuf {
        self.frames
            .last()
            .map(|frame| frame.module.clone())
            .unwrap_or_default()
    }

    fn eval_call(
        &mut self,
        callee: &Expression<'arena>,
        args: &'arena [&'arena Expression<'arena>],
        span: miette::SourceSpan,
    ) -> EvalResult<'arena> {
        let name = match &callee.kind {
            ExpressionKind::Ident {
                name,
                generic_args: None,
            } => *name,
            _ => {
                self.errors.push(Error::Unsupported {
                    message: "only comptime functions can be called in comptime"
                        .to_string()
                        .into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        if self.local_value(name).is_some() {
            self.errors.push(Error::Unsupported {
                message: "comptime values are not callable".to_string().into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let module = self.current_module();

        if !self.is_comptime_fn(&module, name) {
            let label = self.interned(&name);
            self.errors.push(Error::UnresolvedIdent {
                name: label.into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let mut values: Vec<Value<'arena>> = Vec::with_capacity(args.len());
        for arg in args.iter() {
            values.push(self.eval_expr(arg)?);
        }

        self.eval_comptime_call(&module, name, span, &values)
    }

    pub fn eval_expr(&mut self, expr: &Expression<'arena>) -> EvalResult<'arena> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            self.errors.push(Error::LimitExceeded {
                message: "comptime step limit exceeded".to_string().into(),
                src: self.current_src(),
                span: expr.span,
            });
            return Err(Unwind::Break);
        }

        if self.depth > DEPTH_LIMIT {
            self.errors.push(Error::LimitExceeded {
                message: "comptime recursion limit exceeded".to_string().into(),
                src: self.current_src(),
                span: expr.span,
            });
            return Err(Unwind::Break);
        }

        match &expr.kind {
            ExpressionKind::Literal(literal) => self.eval_literal(literal, expr.span),

            ExpressionKind::Ident { name, .. } => self.eval_ident_value(*name, expr.span),

            ExpressionKind::Binary { lhs, rhs, op } => self.eval_binary(*op, lhs, rhs, expr.span),

            ExpressionKind::Unary { expr: inner, op } => self.eval_unary(*op, inner, expr.span),

            ExpressionKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let else_statement: Option<&'arena Statement<'arena>> =
                    else_block.iter().copied().next();
                self.eval_if(condition, then_block, else_statement, expr.span)
            }

            ExpressionKind::Block { stmts, trailing } => self.eval_block(stmts, *trailing),

            ExpressionKind::Switch { object, arms } => self.eval_switch(object, arms),

            ExpressionKind::Call { callee, args } => self.eval_call(callee, args, expr.span),

            ExpressionKind::ArrayInit { elements } => {
                let mut items: Vec<Value<'arena>> = Vec::with_capacity(elements.len());
                for element in elements.iter() {
                    items.push(self.eval_expr(element)?);
                }
                Ok(Value::Array(items))
            }

            ExpressionKind::SliceAccess { object, index } => {
                let obj_value = self.eval_expr(object)?;
                let idx_value = self.eval_expr(index)?;
                match (obj_value, idx_value) {
                    (Value::Array(items), Value::Int(idx)) => {
                        if idx < 0 || idx >= items.len() as i128 {
                            self.errors.push(Error::TypeMismatch {
                                message: format!(
                                    "array index {idx} out of bounds (length {})",
                                    items.len()
                                )
                                .into(),
                                src: self.current_src(),
                                span: expr.span,
                            });
                            return Err(Unwind::Break);
                        }
                        Ok(items[idx as usize].clone())
                    }
                    _ => {
                        self.errors.push(Error::TypeMismatch {
                            message: "comptime slice access expects an array and an integer index"
                                .into(),
                            src: self.current_src(),
                            span: expr.span,
                        });
                        Err(Unwind::Break)
                    }
                }
            }

            ExpressionKind::MacroCall { name, args } => {
                let macro_name_owned = self.interner.borrow().resolve(&name.0).to_owned();
                match macro_name_owned.as_str() {
                    "format" => self.eval_macro_format(name.1, args),
                    "print" => {
                        self.eval_macro_println(name.1, args, false)?;
                        Ok(Value::Bool(false))
                    }
                    "println" => {
                        self.eval_macro_println(name.1, args, true)?;
                        Ok(Value::Bool(false))
                    }
                    "dbg" => self.eval_macro_dbg(name.1, args),
                    "panic" => self.eval_macro_panic(name.1, args),
                    "zeen_type" => self.eval_macro_zeen_type(name.1, args),
                    "zeen_expr" => self.eval_macro_zeen_expr(name.1, args),
                    "zeen_decl" => self.eval_macro_zeen_decl(name.1, args),
                    "emit" => self.eval_macro_emit(name.1, args),
                    "fields" => self.eval_macro_fields(name.1, args),
                    "variants" => self.eval_macro_variants(name.1, args),
                    "typeInfo" => self.eval_macro_type_info(name.1, args),
                    "hasField" => self.eval_macro_has_field(name.1, args),
                    "fieldType" => self.eval_macro_field_type(name.1, args),
                    "fieldName" => self.eval_macro_field_name(name.1, args),
                    "fieldTypeIndex" => self.eval_macro_field_type_index(name.1, args),
                    _ => {
                        self.errors.push(Error::Unsupported {
                            message: format!(
                                "macro `@{macro_name_owned}` is not supported in comptime"
                            )
                            .into(),
                            src: self.current_src(),
                            span: expr.span,
                        });
                        Err(Unwind::Break)
                    }
                }
            }

            ExpressionKind::Splice { expr } => self.eval_expr(expr),

            _ => {
                self.errors.push(Error::Unsupported {
                    message: "expression is not supported in comptime".to_string().into(),
                    src: self.current_src(),
                    span: expr.span,
                });
                Err(Unwind::Break)
            }
        }
    }

    fn eval_literal(&mut self, literal: &Literal, span: miette::SourceSpan) -> EvalResult<'arena> {
        match literal {
            Literal::Int(v) => Ok(Value::Int(*v as i128)),
            Literal::Float(v) => Ok(Value::Float(*v)),
            Literal::Char(v) => Ok(Value::Char(*v)),
            Literal::ByteChar(v) => Ok(Value::Char(*v)),
            Literal::Bool(v) => Ok(Value::Bool(*v)),
            Literal::String(spur) => {
                let text = self.interned(spur);
                Ok(Value::Str(text.into()))
            }
            Literal::Null => {
                let _ = span;
                self.errors.push(Error::Unsupported {
                    message: "comptime expressions cannot evaluate null"
                        .to_string()
                        .into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    fn eval_binary(
        &mut self,
        op: BinaryOp,
        lhs: &Expression<'arena>,
        rhs: &Expression<'arena>,
        span: miette::SourceSpan,
    ) -> EvalResult<'arena> {
        match op {
            BinaryOp::LogicalAnd => {
                let lhs_value = self.eval_expr(lhs)?;
                if self.value_truthy(lhs_value, span, "the `&&` left side/") {
                    return self.eval_expr(rhs);
                }
                Ok(Value::Bool(false))
            }

            BinaryOp::LogicalOr => {
                let lhs_value = self.eval_expr(lhs)?;
                if !self.value_truthy(lhs_value, span, "the `||` left side/") {
                    return Ok(Value::Bool(true));
                }

                self.eval_expr(rhs)
            }

            _ => {
                let lhs_value = self.eval_expr(lhs)?;
                let rhs_value = self.eval_expr(rhs)?;

                self.apply_binary(op, lhs_value, rhs_value, span)
            }
        }
    }

    fn eval_unary(
        &mut self,
        op: UnaryOp,
        inner: &Expression<'arena>,
        span: miette::SourceSpan,
    ) -> EvalResult<'arena> {
        let value = self.eval_expr(inner)?;

        match (op, value) {
            (UnaryOp::Neg, Value::Int(v)) => Ok(Value::Int(-v)),
            (UnaryOp::Neg, Value::Float(v)) => Ok(Value::Float(-v)),
            (UnaryOp::Not, Value::Bool(v)) => Ok(Value::Bool(!v)),
            (UnaryOp::BitNot, Value::Int(v)) => Ok(Value::Int(!v)),

            (op, value) => {
                self.errors.push(Error::TypeMismatch {
                    message: format!(
                        "comptime unary `{}` is not applicable to {}",
                        op,
                        value.type_name() // value is moved, but the `else` arm runs only on rejection
                    )
                    .into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    fn eval_if(
        &mut self,
        condition: &Expression<'arena>,
        then_block: &'arena Statement<'arena>,
        else_block: Option<&'arena Statement<'arena>>,
        span: miette::SourceSpan,
    ) -> EvalResult<'arena> {
        let condition_value = self.eval_expr(condition)?;

        if !self.value_truthy(condition_value, span, "comptime `if` condition") {
            match else_block {
                Some(else_statement) => match self.eval_statement(else_statement) {
                    Ok(value) => return Ok(value.unwrap_or(Value::Bool(false))),
                    Err(Unwind::Return(v)) => return Err(Unwind::Return(v)),
                    Err(_) => return Err(Unwind::Break),
                },
                None => return Ok(Value::Bool(false)),
            };
        }

        match self.eval_statement(then_block) {
            Ok(value) => Ok(value.unwrap_or(Value::Bool(false))),
            Err(Unwind::Return(v)) => Err(Unwind::Return(v)),
            Err(_) => Err(Unwind::Break),
        }
    }

    fn eval_block(
        &mut self,
        stmts: &'arena [&Statement<'arena>],
        trailing: Option<&Expression<'arena>>,
    ) -> EvalResult<'arena> {
        self.frames
            .last_mut()
            .expect("comptime frame")
            .scopes
            .push(FxHashMap::default());

        let outcome = self.eval_statements(stmts);

        {
            let frame = self.frames.last_mut().expect("comptime frame");
            frame.scopes.pop();
        }

        match outcome {
            Ok(Some(value)) => return Ok(value),
            Ok(None) => {}
            Err(Unwind::Return(v)) => return Err(Unwind::Return(v)),
            Err(_) => return Err(Unwind::Break),
        }

        match trailing {
            Some(trailing_expr) => self.eval_expr(trailing_expr),
            None => Ok(Value::Bool(false)),
        }
    }

    fn eval_switch(
        &mut self,
        object: &'arena Expression<'arena>,
        arms: &'arena [&'arena zeen_ast::expressions::Arm<'arena>],
    ) -> EvalResult<'arena> {
        let scrutinee = self.eval_expr(object)?;

        for arm in arms {
            if let Some(_value) = self.switch_pattern_matches(&arm.pattern, &scrutinee)? {
                let arm_body_value = self.eval_expr(arm.body)?;
                return Ok(arm_body_value);
            }
        }

        self.errors.push(Error::TypeMismatch {
            message: "comptime `switch` must have a `_` wildcard arm"
                .to_string()
                .into(),
            src: self.current_src(),
            span: object.span,
        });
        Err(Unwind::Break)
    }

    fn switch_pattern_matches(
        &mut self,
        pattern: &'arena zeen_ast::expressions::Pattern<'arena>,
        scrutinee: &Value<'arena>,
    ) -> Result<Option<()>, Unwind<'arena>> {
        use zeen_ast::expressions::Pattern;

        match pattern {
            Pattern::Wildcard => Ok(Some(())),

            Pattern::Literal(literal) => {
                let literal_value =
                    self.eval_literal(literal, miette::SourceSpan::new(0.into(), 0))?;
                if self.value_equals(&literal_value, scrutinee) {
                    Ok(Some(()))
                } else {
                    Ok(None)
                }
            }

            Pattern::Range {
                start,
                end,
                inclusive,
            } => {
                let value = match scrutinee {
                    Value::Int(v) => v,
                    _ => {
                        self.errors.push(Error::TypeMismatch {
                            message: "comptime `switch` range patterns only cover integers"
                                .to_string()
                                .into(),
                            src: self.current_src(),
                            span: miette::SourceSpan::new(0.into(), 0),
                        });
                        return Err(Unwind::Break);
                    }
                };

                let lower = match start {
                    Some(Literal::Int(v)) => *v as i128,
                    None => i128::MIN,
                    _ => {
                        self.errors.push(Error::TypeMismatch {
                            message: "comptime `switch` range patterns only cover integer bounds"
                                .to_string()
                                .into(),
                            src: self.current_src(),
                            span: miette::SourceSpan::new(0.into(), 0),
                        });
                        return Err(Unwind::Break);
                    }
                };

                let upper = match end {
                    Some(Literal::Int(v)) => *v as i128,
                    None => i128::MAX,
                    _ => {
                        self.errors.push(Error::TypeMismatch {
                            message: "comptime `switch` range patterns only cover integer bounds"
                                .to_string()
                                .into(),
                            src: self.current_src(),
                            span: miette::SourceSpan::new(0.into(), 0),
                        });
                        return Err(Unwind::Break);
                    }
                };

                let ok = if *inclusive {
                    *value >= lower && *value <= upper
                } else {
                    *value >= lower && *value < upper
                };

                if ok { Ok(Some(())) } else { Ok(None) }
            }

            Pattern::Or(patterns) => {
                for pattern in patterns.iter() {
                    if self.switch_pattern_matches(pattern, scrutinee)?.is_some() {
                        return Ok(Some(()));
                    }
                }

                Ok(None)
            }

            Pattern::Named { .. } | Pattern::EnumVariant { .. } => {
                self.errors.push(Error::Unsupported {
                    message: "comptime `switch` does not support named or enum patterns"
                        .to_string()
                        .into(),
                    src: self.current_src(),
                    span: miette::SourceSpan::new(0.into(), 0),
                });
                Err(Unwind::Break)
            }
        }
    }

    fn value_truthy(&mut self, value: Value<'arena>, span: miette::SourceSpan, what: &str) -> bool {
        match value {
            Value::Bool(v) => v,
            other => {
                self.errors.push(Error::TypeMismatch {
                    message: format!("`{}` expects bool, got {}", what, other.type_name()).into(),
                    src: self.current_src(),
                    span,
                });
                false
            }
        }
    }

    fn value_equals(&self, lhs: &Value<'arena>, rhs: &Value<'arena>) -> bool {
        match (lhs, rhs) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Char(a), Value::Char(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            _ => false,
        }
    }

    fn apply_binary(
        &mut self,
        op: BinaryOp,
        lhs: Value<'arena>,
        rhs: Value<'arena>,
        span: miette::SourceSpan,
    ) -> EvalResult<'arena> {
        let outcome = match (op, lhs, rhs) {
            (BinaryOp::Add, Value::Int(a), Value::Int(b)) => a.checked_add(b).map(Value::Int),
            (BinaryOp::Sub, Value::Int(a), Value::Int(b)) => a.checked_sub(b).map(Value::Int),
            (BinaryOp::Mul, Value::Int(a), Value::Int(b)) => a.checked_mul(b).map(Value::Int),
            (BinaryOp::Div, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    None
                } else {
                    a.checked_div(b).map(Value::Int)
                }
            }
            (BinaryOp::Mod, Value::Int(a), Value::Int(b)) => {
                if b == 0 {
                    None
                } else {
                    a.checked_rem(b).map(Value::Int)
                }
            }

            (BinaryOp::Add, Value::Float(a), Value::Float(b)) => Some(Value::Float(a + b)),
            (BinaryOp::Sub, Value::Float(a), Value::Float(b)) => Some(Value::Float(a - b)),
            (BinaryOp::Mul, Value::Float(a), Value::Float(b)) => Some(Value::Float(a * b)),
            (BinaryOp::Div, Value::Float(a), Value::Float(b)) => Some(Value::Float(a / b)),

            (BinaryOp::Add, Value::Str(a), Value::Str(b)) => {
                Some(Value::Str(format!("{}{}", a, b).into()))
            }

            (BinaryOp::Eq, Value::Int(a), Value::Int(b)) => Some(Value::Bool(a == b)),
            (BinaryOp::Ne, Value::Int(a), Value::Int(b)) => Some(Value::Bool(a != b)),
            (BinaryOp::Lt, Value::Int(a), Value::Int(b)) => Some(Value::Bool(a < b)),
            (BinaryOp::Gt, Value::Int(a), Value::Int(b)) => Some(Value::Bool(a > b)),
            (BinaryOp::Le, Value::Int(a), Value::Int(b)) => Some(Value::Bool(a <= b)),
            (BinaryOp::Ge, Value::Int(a), Value::Int(b)) => Some(Value::Bool(a >= b)),

            (BinaryOp::Eq, Value::Float(a), Value::Float(b)) => Some(Value::Bool(a == b)),
            (BinaryOp::Ne, Value::Float(a), Value::Float(b)) => Some(Value::Bool(a != b)),
            (BinaryOp::Lt, Value::Float(a), Value::Float(b)) => Some(Value::Bool(a < b)),
            (BinaryOp::Gt, Value::Float(a), Value::Float(b)) => Some(Value::Bool(a > b)),
            (BinaryOp::Le, Value::Float(a), Value::Float(b)) => Some(Value::Bool(a <= b)),
            (BinaryOp::Ge, Value::Float(a), Value::Float(b)) => Some(Value::Bool(a >= b)),

            (BinaryOp::Eq, Value::Bool(a), Value::Bool(b)) => Some(Value::Bool(a == b)),
            (BinaryOp::Ne, Value::Bool(a), Value::Bool(b)) => Some(Value::Bool(a != b)),

            (BinaryOp::Eq, Value::Char(a), Value::Char(b)) => Some(Value::Bool(a == b)),
            (BinaryOp::Ne, Value::Char(a), Value::Char(b)) => Some(Value::Bool(a != b)),

            (BinaryOp::Eq, Value::Str(a), Value::Str(b)) => Some(Value::Bool(a == b)),
            (BinaryOp::Ne, Value::Str(a), Value::Str(b)) => Some(Value::Bool(a != b)),

            (BinaryOp::BitAnd, Value::Int(a), Value::Int(b)) => Some(Value::Int(a & b)),
            (BinaryOp::BitOr, Value::Int(a), Value::Int(b)) => Some(Value::Int(a | b)),
            (BinaryOp::BitXor, Value::Int(a), Value::Int(b)) => Some(Value::Int(a ^ b)),
            (BinaryOp::Shl, Value::Int(a), Value::Int(b)) => a
                .checked_shl(u32::try_from(b).unwrap_or(u32::MAX))
                .map(Value::Int),
            (BinaryOp::Shr, Value::Int(a), Value::Int(b)) => a
                .checked_shr(u32::try_from(b).unwrap_or(u32::MAX))
                .map(Value::Int),

            (_, lhs, rhs) => {
                self.errors.push(Error::TypeMismatch {
                    message: format!(
                        "binary `{}` cannot apply to {} and {}",
                        op,
                        lhs.type_name(),
                        rhs.type_name()
                    )
                    .into(),
                    src: self.current_src(),
                    span,
                });
                None
            }
        };

        match outcome {
            Some(value) => Ok(value),
            None => {
                self.errors.push(Error::TypeMismatch {
                    message: "arithmetic overflow in comptime evaluation"
                        .to_string()
                        .into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    fn eval_macro_format(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let format_expr = match args.first() {
            Some(e) => e,
            None => {
                self.errors.push(Error::TypeMismatch {
                    message: "@format requires at least one argument (format string)".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let format_str = match &format_expr.kind {
            ExpressionKind::Literal(Literal::String(spur)) => {
                self.interner.borrow().resolve(spur).to_string()
            }
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@format first argument must be a string literal".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let mut result = String::new();
        let mut args_iter = args.iter().skip(1).peekable();
        let mut chars = format_str.chars();

        while let Some(c) = chars.next() {
            if c == '{' {
                match chars.next() {
                    Some('}') => {
                        if let Some(arg) = args_iter.next() {
                            let value = self.eval_expr(arg)?;
                            result.push_str(&value.display());
                        } else {
                            result.push_str("{}");
                        }
                    }
                    Some('{') => result.push('{'),
                    Some(_) => {
                        result.push_str("{}");
                    }
                    None => result.push('{'),
                }
            } else if c == '}' {
                match chars.next() {
                    Some('}') => result.push('}'),
                    Some(_) => {
                        result.push('}');
                    }
                    None => result.push('}'),
                }
            } else {
                result.push(c);
            }
        }

        Ok(Value::Str(result.into()))
    }

    fn eval_macro_println(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
        with_newline: bool,
    ) -> Result<(), Unwind<'arena>> {
        if args.is_empty() {
            return Ok(());
        }

        let (call_name, call_span) = self.frames.last().and_then(|f| f.caller_name).unzip();
        let source = self.current_src();
        let source_name = source.name().to_string();

        let format_str = match &args[0].kind {
            ExpressionKind::Literal(Literal::String(spur)) => {
                self.interner.borrow().resolve(spur).to_string()
            }
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@println first argument must be a string literal".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let mut result = String::new();
        let mut chars = format_str.chars();

        while let Some(c) = chars.next() {
            if c == '{' {
                match chars.next() {
                    Some('}') => {
                        for (i, arg) in args.iter().skip(1).copied().enumerate() {
                            if i > 0 {
                                result.push(' ');
                            }
                            if let Ok(value) = self.eval_expr(arg) {
                                result.push_str(&value.display());
                            }
                        }
                    }
                    Some('{') => result.push('{'),
                    Some(_) => result.push_str("{}"),
                    None => result.push('{'),
                }
            } else {
                result.push(c);
            }
        }

        if let Some(name) = call_name {
            let name_str = self.interner.borrow().resolve(&name).to_string();
            let offset = call_span.unwrap().offset();
            let source_str = source.inner();
            let source_text = source_str.as_ref();
            let line = source_text[..offset].matches('\n').count() + 1;
            let prefix = format!("[comptime {}][{}:{}]", name_str, source_name, line);
            if with_newline {
                println!("{} {}", prefix, result);
            } else {
                print!("{} {}", prefix, result);
            }
        } else {
            if with_newline {
                println!("{}", result);
            } else {
                print!("{}", result);
            }
        }

        Ok(())
    }

    fn eval_macro_dbg(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        if args.is_empty() {
            self.errors.push(Error::TypeMismatch {
                message: "@dbg requires at least one argument".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let expr = args[0];
        let value = self.eval_expr(expr)?;
        let (call_name, call_span) = self.frames.last().and_then(|f| f.caller_name).unzip();
        let source = self.current_src();
        let source_name = source.name().to_string();

        let (prefix, value_str) = if let Some(name) = call_name {
            let name_str = self.interner.borrow().resolve(&name).to_string();
            let offset = call_span.unwrap().offset();
            let source_str = source.inner();
            let source_text = source_str.as_ref();
            let line = source_text[..offset].matches('\n').count() + 1;
            let prefix = format!("[comptime {}][{}:{}]", name_str, source_name, line);
            (prefix, value.display())
        } else {
            ("[comptime dbg]".to_string(), value.display())
        };

        println!("{} {}", prefix, value_str);
        Ok(value)
    }

    fn eval_macro_panic(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let message = if args.is_empty() {
            "comptime panic".into()
        } else {
            match &args[0].kind {
                ExpressionKind::Literal(Literal::String(spur)) => {
                    let format_str = self.interner.borrow().resolve(spur).to_string();
                    let mut result = String::new();
                    let mut chars = format_str.chars();
                    let mut args_iter = args.iter().skip(1).peekable();

                    while let Some(c) = chars.next() {
                        if c == '{' {
                            match chars.next() {
                                Some('}') => {
                                    if let Some(arg) = args_iter.next() {
                                        let value = self.eval_expr(arg)?;
                                        result.push_str(&value.display());
                                    } else {
                                        result.push_str("{}");
                                    }
                                }
                                Some('{') => result.push('{'),
                                Some(_) => result.push_str("{}"),
                                None => result.push('{'),
                            }
                        } else if c == '}' {
                            match chars.next() {
                                Some('}') => result.push('}'),
                                Some(_) => {
                                    result.push('}');
                                }
                                None => result.push('}'),
                            }
                        } else {
                            result.push(c);
                        }
                    }
                    result
                }
                _ => "comptime panic".into(),
            }
        };

        self.errors.push(Error::Panic {
            message: message.into(),
            trace: Vec::new(),
            src: self.current_src(),
            span,
        });
        Err(Unwind::Break)
    }

    fn eval_macro_zeen_type(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let type_expr = match args.first() {
            Some(e) => e,
            None => {
                self.errors.push(Error::TypeMismatch {
                    message: "@zeen_type requires a type argument".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let ty = match &type_expr.kind {
            ExpressionKind::Type(ty) => *ty,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@zeen_type requires a type expression".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        Ok(Value::Type(ty))
    }

    fn eval_macro_zeen_expr(
        &mut self,
        _span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let expr = match args.first() {
            Some(e) => *e,
            None => {
                self.errors.push(Error::TypeMismatch {
                    message: "@zeen_expr requires an expression argument".into(),
                    src: self.current_src(),
                    span: _span,
                });
                return Err(Unwind::Break);
            }
        };

        Ok(Value::Expr(expr))
    }

    fn eval_macro_zeen_decl(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let name = match &args.first().map(|e| &e.kind) {
            Some(ExpressionKind::Ident { name, .. }) => *name,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@zeen_decl requires a declaration name".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let module = self.current_module();
        let decl = self.scopes.get(&module).and_then(|scope| {
            scope
                .fns
                .get(&name)
                .copied()
                .or_else(|| scope.structs.get(&name).copied())
                .or_else(|| scope.enums.get(&name).copied())
        });

        match decl {
            Some(decl) => Ok(Value::Decl(decl)),
            None => {
                let label = self.interned(&name);
                self.errors.push(Error::UnresolvedIdent {
                    name: label.into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    fn eval_macro_emit(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let value = match args.first() {
            Some(e) => self.eval_expr(e)?,
            None => {
                self.errors.push(Error::TypeMismatch {
                    message: "@emit requires a declaration argument".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        match value {
            Value::Decl(decl) => {
                match decl.kind {
                    DeclarationKind::ComptimeFn {
                        name,
                        params,
                        return_type,
                        body,
                        is_pub,
                        ..
                    } => {
                        let fn_decl = DeclarationKind::FnDecl {
                            name,
                            generics: None,
                            params,
                            return_type,
                            body,
                            is_pub,
                            is_extern: false,
                        };
                        self.emitted_decls.push(self.arena.alloc(Declaration {
                            kind: fn_decl,
                            source: decl.source.clone(),
                        }));
                    }
                    DeclarationKind::ComptimeConst { .. } => {
                        self.errors.push(Error::Unsupported {
                            message: "@emit with comptime const is not supported in v1".into(),
                            src: self.current_src(),
                            span,
                        });
                        return Err(Unwind::Break);
                    }
                    _ => {
                        self.errors.push(Error::TypeMismatch {
                            message: "@emit expects a comptime fn declaration".into(),
                            src: self.current_src(),
                            span,
                        });
                        return Err(Unwind::Break);
                    }
                }
                Ok(Value::Bool(false))
            }
            other => {
                self.errors.push(Error::TypeMismatch {
                    message: format!("@emit expects a zeen_decl value, got {}", other.type_name())
                        .into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    fn eval_macro_fields(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let ty_expr = match args.first() {
            Some(e) => e,
            None => {
                self.errors.push(Error::TypeMismatch {
                    message: "@fields requires a type argument".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let decl = self.resolve_type_decl(ty_expr, span)?;
        let count = match &decl.kind {
            DeclarationKind::StructDecl { fields, .. } => fields.len() as i128,
            DeclarationKind::EnumDecl { variants, .. } => variants.len() as i128,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@fields expects a struct or enum type".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        Ok(Value::Int(count))
    }

    fn eval_macro_variants(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let ty_expr = match args.first() {
            Some(e) => e,
            None => {
                self.errors.push(Error::TypeMismatch {
                    message: "@variants requires a type argument".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let decl = self.resolve_type_decl(ty_expr, span)?;
        let count = match &decl.kind {
            DeclarationKind::EnumDecl { variants, .. } => variants.len() as i128,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@variants expects an enum type".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        Ok(Value::Int(count))
    }

    fn eval_macro_type_info(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let ty_expr = match args.first() {
            Some(e) => e,
            None => {
                self.errors.push(Error::TypeMismatch {
                    message: "@typeInfo requires a type argument".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let decl = self.resolve_type_decl(ty_expr, span)?;
        let info_str = match &decl.kind {
            DeclarationKind::StructDecl { .. } => "struct",
            DeclarationKind::EnumDecl { .. } => "enum",
            DeclarationKind::FnDecl { .. } => "fn",
            DeclarationKind::ComptimeFn { .. } => "comptime_fn",
            DeclarationKind::Alias { .. } => "alias",
            DeclarationKind::GlobalVar { .. } => "global_var",
            DeclarationKind::ExternVar { .. } => "extern_var",
            DeclarationKind::ImplementDecl { .. } => "implement",
            DeclarationKind::InterfaceDecl { .. } => "interface",
            DeclarationKind::Use { .. } => "use",
            DeclarationKind::ComptimeConst { .. } => "comptime_const",
            DeclarationKind::ComptimeBlock { .. } => "comptime_block",
            DeclarationKind::ConditionalBlock { .. } => "conditional_block",
            DeclarationKind::ExternLink { .. } | DeclarationKind::ExternInclude { .. } => "extern",
        };

        Ok(Value::Str(info_str.into()))
    }

    fn eval_macro_has_field(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        if args.len() < 2 {
            self.errors.push(Error::TypeMismatch {
                message: "@hasField requires a type and a field name".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let ty_expr = &args[0];
        let field_name_expr = &args[1];

        let field_name = match &field_name_expr.kind {
            ExpressionKind::Literal(Literal::String(spur)) => *spur,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@hasField second argument must be a string literal".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let decl = self.resolve_type_decl(ty_expr, span)?;
        let has = match &decl.kind {
            DeclarationKind::StructDecl { fields, .. } => fields
                .iter()
                .any(|f| self.interned(&f.name) == self.interned(&field_name)),
            _ => false,
        };

        Ok(Value::Bool(has))
    }

    fn eval_macro_field_type(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        if args.len() < 2 {
            self.errors.push(Error::TypeMismatch {
                message: "@fieldType requires a type and a field name".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let ty_expr = &args[0];
        let field_name_expr = &args[1];

        let field_name = match &field_name_expr.kind {
            ExpressionKind::Literal(Literal::String(spur)) => *spur,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@fieldType second argument must be a string literal".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let decl = self.resolve_type_decl(ty_expr, span)?;
        let field_ty = match &decl.kind {
            DeclarationKind::StructDecl { fields, .. } => fields
                .iter()
                .find(|f| self.interned(&f.name) == self.interned(&field_name))
                .map(|f| f.ty),
            _ => None,
        };

        match field_ty {
            Some(ty) => Ok(Value::Type(ty)),
            None => {
                self.errors.push(Error::TypeMismatch {
                    message: format!("field not found: {}", self.interned(&field_name)).into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    fn eval_macro_field_name(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        if args.len() < 2 {
            self.errors.push(Error::TypeMismatch {
                message: "@fieldName requires a type and a field index".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let ty_expr = &args[0];
        let index_expr = &args[1];

        let index = match &index_expr.kind {
            ExpressionKind::Literal(Literal::Int(v)) => *v as usize,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@fieldName second argument must be an integer literal".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let decl = self.resolve_type_decl(ty_expr, span)?;
        let fields = match &decl.kind {
            DeclarationKind::StructDecl { fields, .. } => fields,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@fieldName expects a struct type".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        if index >= fields.len() {
            self.errors.push(Error::TypeMismatch {
                message: format!(
                    "field index {index} out of bounds (struct has {} fields)",
                    fields.len()
                )
                .into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let name_str = self.interned(&fields[index].name);
        Ok(Value::Str(name_str.into()))
    }

    fn eval_macro_field_type_index(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        if args.len() < 2 {
            self.errors.push(Error::TypeMismatch {
                message: "@fieldTypeIndex requires a type and a field index".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let ty_expr = &args[0];
        let index_expr = &args[1];

        let index = match &index_expr.kind {
            ExpressionKind::Literal(Literal::Int(v)) => *v as usize,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@fieldTypeIndex second argument must be an integer literal".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let decl = self.resolve_type_decl(ty_expr, span)?;
        let field_ty = match &decl.kind {
            DeclarationKind::StructDecl { fields, .. } => {
                if index >= fields.len() {
                    None
                } else {
                    Some(fields[index].ty)
                }
            }
            _ => None,
        };

        match field_ty {
            Some(ty) => Ok(Value::Type(ty)),
            None => {
                self.errors.push(Error::TypeMismatch {
                    message: format!("field index {index} out of bounds").into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    fn resolve_type_decl(
        &mut self,
        ty_expr: &'arena Expression<'arena>,
        span: miette::SourceSpan,
    ) -> Result<&'arena Declaration<'arena>, Unwind<'arena>> {
        let ty = match &ty_expr.kind {
            ExpressionKind::Type(ty) => *ty,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "expected a type expression".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let name = match &ty.kind {
            zeen_ast::types::TypeKind::Named { name, .. } => *name,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "expected a named type".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let module = self.current_module();
        let Some(scope) = self.scopes.get(&module) else {
            self.errors.push(Error::UnresolvedIdent {
                name: "<module>".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        };

        scope
            .structs
            .get(&name)
            .copied()
            .or_else(|| scope.enums.get(&name).copied())
            .ok_or_else(|| {
                let label = self.interned(&name);
                self.errors.push(Error::UnresolvedIdent {
                    name: label.into(),
                    src: self.current_src(),
                    span,
                });
                Unwind::Break
            })
    }
}
