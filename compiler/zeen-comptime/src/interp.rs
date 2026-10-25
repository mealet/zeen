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
    Declaration, DeclarationKind, Expression, ExpressionKind, Statement, StatementKind, TypeExpr,
    declarations::{EnumVariantPayload, StructField},
    expressions::{BinaryOp, Literal, UnaryOp},
    types::TypeKind,
};

use crate::error::Error;
use crate::format::{FormatChunk, FormatSpec, parse_format_string};
use crate::value::{Record, Value};

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

pub(crate) enum FrameContext {
    None,
    Block {
        file: String,
        line: usize,
    },
    Function {
        name: Spur,
        file: String,
        line: usize,
    },
}

pub struct CallFrame<'arena> {
    pub(crate) module: PathBuf,
    pub(crate) src: NamedSource<Arc<String>>,
    pub(crate) scopes: EvalFrame<'arena>,
    pub(crate) context: FrameContext,
}

fn source_line(src: &NamedSource<Arc<String>>, span: miette::SourceSpan) -> usize {
    let text = src.inner().as_str();
    let offset = span.offset().min(text.len());
    text[..offset].matches('\n').count() + 1
}

fn source_site(src: &NamedSource<Arc<String>>, span: miette::SourceSpan) -> (String, usize) {
    (src.name().to_string(), source_line(src, span))
}

pub struct Interp<'arena> {
    pub(crate) arena: &'arena Bump,
    pub(crate) interner: Rc<RefCell<Rodeo>>,

    pub(crate) scopes: FxHashMap<PathBuf, ModuleScope<'arena>>,

    pub(crate) frames: Vec<CallFrame<'arena>>,

    pub(crate) default_src: Option<NamedSource<Arc<String>>>,

    pub(crate) shadow_stack: Vec<FxHashSet<Spur>>,

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
                    let src = decl.source.src();
                    let (file, line) = source_site(&src, decl.source.span);

                    self.frames.push(CallFrame {
                        module: path.clone(),
                        src,
                        scopes: vec![FxHashMap::default()],
                        context: FrameContext::Block { file, line },
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

        Ok(self.arena.alloc_slice_copy(&kept))
    }

    pub(crate) fn interned(&self, spur: &Spur) -> String {
        self.interner.borrow().resolve(spur).to_string()
    }

    pub(crate) fn type_display(&self, ty: &TypeExpr<'arena>) -> String {
        match &ty.kind {
            TypeKind::Builtin(builtin) => builtin.to_string(),

            TypeKind::SelfType | TypeKind::SelfAlias => "Self".to_string(),

            TypeKind::VaArgs => "...".to_string(),

            TypeKind::Named { name, generic_args } => match generic_args {
                Some(args) => {
                    let rendered: Vec<String> =
                        args.iter().map(|arg| self.type_display(arg)).collect();
                    format!("{}[{}]", self.interned(name), rendered.join(", "))
                }
                None => self.interned(name),
            },

            TypeKind::Const(inner) => self.type_display(inner),

            TypeKind::TypeOf(_) => "typeof".to_string(),

            TypeKind::SinglePointer(inner) => format!("*{}", self.type_display(inner)),

            TypeKind::ManyPointer(inner) => format!("[*]{}", self.type_display(inner)),

            TypeKind::Array { element, len } => match len {
                Some(len) => format!("[{}]{}", self.expr_display(len), self.type_display(element)),
                None => format!("[]{}", self.type_display(element)),
            },

            TypeKind::Fn { params, ret, .. } => {
                let rendered: Vec<String> = params
                    .iter()
                    .map(|param| self.type_display(param))
                    .collect();
                format!("fn({}) {}", rendered.join(", "), self.type_display(ret))
            }

            TypeKind::FatFn { params, ret, once } => {
                let rendered: Vec<String> = params
                    .iter()
                    .map(|param| self.type_display(param))
                    .collect();
                let name = if *once { "FnOnce" } else { "Fn" };
                format!(
                    "{}({}) {}",
                    name,
                    rendered.join(", "),
                    self.type_display(ret)
                )
            }
        }
    }

    fn expr_display(&self, expr: &Expression<'arena>) -> String {
        match &expr.kind {
            ExpressionKind::Literal(Literal::Int(value)) => value.to_string(),
            ExpressionKind::Ident { name, .. } => self.interned(name),
            _ => "_".to_string(),
        }
    }

    pub(crate) fn display_value(&self, value: &Value<'arena>) -> String {
        match value {
            Value::Type(ty) => self.type_display(ty),
            Value::Array(items) => {
                let rendered: Vec<String> =
                    items.iter().map(|item| self.display_value(item)).collect();
                format!("[{}]", rendered.join(", "))
            }
            Value::Record(record) => {
                let rendered: Vec<String> = record
                    .fields
                    .iter()
                    .map(|(name, item)| format!("{name}: {}", self.display_value(item)))
                    .collect();
                format!("{} {{ {} }}", record.kind, rendered.join(", "))
            }
            other => other.display(),
        }
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

        let caller_src = self.current_src();
        let (file, line) = source_site(&caller_src, span);

        self.frames.push(CallFrame {
            module: module.clone(),
            src: fn_src,
            scopes: vec![bindings],
            context: FrameContext::Function { name, file, line },
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
            context: FrameContext::None,
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

        let outcome = match iterator_value {
            Value::Int(total) if total >= 0 => {
                let values: Vec<Value<'arena>> = (0..total).map(Value::Int).collect();
                self.run_for_iteration(name, values, block)
            }

            Value::Array(items) => self.run_for_iteration(name, items, block),

            Value::Str(text) => {
                let chars: Vec<Value<'arena>> = text.chars().map(Value::Char).collect();
                self.run_for_iteration(name, chars, block)
            }

            other => {
                self.errors.push(Error::TypeMismatch {
                    message: format!(
                        "comptime `for` supports integer ranges, arrays and strings, got {}",
                        other.type_name()
                    )
                    .into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        };

        self.depth -= 1;

        match outcome {
            Ok(()) => Ok(None),
            Err(unwind) => Err(unwind),
        }
    }

    fn run_for_iteration(
        &mut self,
        name: Spur,
        values: Vec<Value<'arena>>,
        block: &'arena Statement<'arena>,
    ) -> Result<(), Unwind<'arena>> {
        for value in values {
            {
                let frame = self.frames.last_mut().expect("comptime frame");
                let scope = frame.scopes.last_mut().expect("comptime scope exists");
                scope.insert(name, value);
            }

            match self.eval_statement(block) {
                Err(Unwind::Return(v)) => return Err(Unwind::Return(v)),
                Err(Unwind::Break) => break,
                Err(Unwind::Continue) => continue,
                Ok(_) => {}
            }
        }

        Ok(())
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

            ExpressionKind::ComptimeBlock { stmts, trailing } => {
                self.eval_comptime_block(stmts, *trailing, expr.span)
            }

            ExpressionKind::Switch { object, arms } => self.eval_switch(object, arms),

            ExpressionKind::Call { callee, args } => self.eval_call(callee, args, expr.span),

            ExpressionKind::ArrayInit { elements } => {
                let mut items: Vec<Value<'arena>> = Vec::with_capacity(elements.len());
                for element in elements.iter() {
                    items.push(self.eval_expr(element)?);
                }
                Ok(Value::Array(items))
            }

            ExpressionKind::Type(ty) => Ok(Value::Type(ty)),

            ExpressionKind::FieldAccess { object, field } => {
                let target = self.eval_expr(object)?;

                let field_name = match &field.kind {
                    ExpressionKind::Ident { name, .. } => self.interned(name),
                    _ => {
                        self.errors.push(Error::TypeMismatch {
                            message: "comptime field access expects a field name".into(),
                            src: self.current_src(),
                            span: expr.span,
                        });
                        return Err(Unwind::Break);
                    }
                };

                match target {
                    Value::Record(record) => match record.get(&field_name) {
                        Some(value) => Ok(value.clone()),
                        None => {
                            self.errors.push(Error::TypeMismatch {
                                message: format!("`{}` has no field `{field_name}`", record.kind)
                                    .into(),
                                src: self.current_src(),
                                span: expr.span,
                            });
                            Err(Unwind::Break)
                        }
                    },
                    other => {
                        self.errors.push(Error::TypeMismatch {
                            message: format!(
                                "field access expects a comptime record, got {}",
                                other.type_name()
                            )
                            .into(),
                            src: self.current_src(),
                            span: expr.span,
                        });
                        Err(Unwind::Break)
                    }
                }
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
                    (Value::Str(text), Value::Int(idx)) => {
                        let len = text.chars().count();

                        if idx < 0 || idx >= len as i128 {
                            self.errors.push(Error::TypeMismatch {
                                message: format!("string index {idx} out of bounds (length {len})")
                                    .into(),
                                src: self.current_src(),
                                span: expr.span,
                            });
                            return Err(Unwind::Break);
                        }

                        match text.chars().nth(idx as usize) {
                            Some(value) => Ok(Value::Char(value)),
                            None => {
                                self.errors.push(Error::TypeMismatch {
                                    message: format!("string index {idx} out of bounds").into(),
                                    src: self.current_src(),
                                    span: expr.span,
                                });
                                Err(Unwind::Break)
                            }
                        }
                    }
                    _ => {
                        self.errors.push(Error::TypeMismatch {
                            message:
                                "comptime slice access expects an array or a string and an integer index"
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
                    "fields" => self.eval_macro_fields(name.1, args),
                    "variants" => self.eval_macro_variants(name.1, args),
                    "typeInfo" => self.eval_macro_type_info(name.1, args),
                    "hasField" => self.eval_macro_has_field(name.1, args),
                    "fieldType" => self.eval_macro_field_type(name.1, args),
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

                if !text.contains("$(") {
                    return Ok(Value::Str(text.into()));
                }

                let module = self.current_module();
                let interpolated = self.interpolate_string(&module, &text, span)?;
                Ok(Value::Str(interpolated.into()))
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

    pub(crate) fn interpolate_string(
        &mut self,
        module: &PathBuf,
        text: &str,
        span: miette::SourceSpan,
    ) -> std::result::Result<String, Unwind<'arena>> {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;

        while i < chars.len() {
            if chars[i] == '$' && chars.get(i + 1) == Some(&'(') {
                let start = i + 2;
                let mut depth = 1usize;
                let mut j = start;

                while j < chars.len() {
                    match chars[j] {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }

                    j += 1;
                }

                if depth != 0 {
                    self.errors.push(Error::TypeMismatch {
                        message: "unterminated `$(` in string literal".into(),
                        src: self.current_src(),
                        span,
                    });
                    return Err(Unwind::Break);
                }

                let fragment: String = chars[start..j].iter().collect();
                let value = self.eval_interpolation_fragment(module, &fragment, span)?;
                out.push_str(&self.display_value(&value));
                i = j + 1;
                continue;
            }

            out.push(chars[i]);
            i += 1;
        }

        Ok(out)
    }

    fn format_string_of(
        &mut self,
        expr: &'arena Expression<'arena>,
        span: miette::SourceSpan,
        macro_name: &str,
    ) -> std::result::Result<String, Unwind<'arena>> {
        let spur = match &expr.kind {
            ExpressionKind::Literal(Literal::String(spur)) => *spur,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: format!("@{macro_name} first argument must be a string literal")
                        .into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let text = self.interned(&spur);

        if !text.contains("$(") {
            return Ok(text);
        }

        let module = self.current_module();
        self.interpolate_string(&module, &text, span)
    }

    fn eval_interpolation_fragment(
        &mut self,
        module: &PathBuf,
        fragment: &str,
        span: miette::SourceSpan,
    ) -> EvalResult<'arena> {
        let src = Arc::new(fragment.to_string());
        let filename = Rc::new(String::from("<comptime interpolation>"));
        let tokens: Vec<_> = zeen_lexer::tokenize(fragment).collect();

        let expr = {
            let mut iter = tokens.into_iter();
            let mut parser = zeen_parser::Parser::new(
                filename,
                src,
                &mut iter,
                self.arena,
                Rc::clone(&self.interner),
            );
            let parsed = zeen_parser::expressions::ExprParser::new(&mut parser).parse();

            match parsed {
                Some(expr) if parser.errors.is_empty() => expr,
                _ => {
                    self.errors.push(Error::TypeMismatch {
                        message: "invalid expression inside `$(` splice".into(),
                        src: self.current_src(),
                        span,
                    });
                    return Err(Unwind::Break);
                }
            }
        };

        self.eval_snippet(module, expr)
    }

    fn eval_snippet(
        &mut self,
        module: &PathBuf,
        expr: &'arena Expression<'arena>,
    ) -> EvalResult<'arena> {
        if self.frames.is_empty() {
            self.eval_fresh_expr(module, expr)
        } else {
            self.eval_expr(expr)
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

        let result = match (outcome, trailing) {
            (Err(Unwind::Return(v)), _) => Err(Unwind::Return(v)),
            (Err(Unwind::Break), _) => Err(Unwind::Break),
            (Err(Unwind::Continue), _) => Err(Unwind::Continue),
            (Ok(_), Some(trailing_expr)) => self.eval_expr(trailing_expr),
            (Ok(Some(value)), None) => Ok(value),
            (Ok(None), None) => Ok(Value::Bool(false)),
        };

        let frame = self.frames.last_mut().expect("comptime frame");
        frame.scopes.pop();

        result
    }

    fn eval_comptime_block(
        &mut self,
        stmts: &'arena [&Statement<'arena>],
        trailing: Option<&Expression<'arena>>,
        span: miette::SourceSpan,
    ) -> EvalResult<'arena> {
        let src = self.current_src();
        let (file, line) = source_site(&src, span);

        self.frames.push(CallFrame {
            module: self.current_module(),
            src,
            scopes: vec![FxHashMap::default()],
            context: FrameContext::Block { file, line },
        });

        let result = self.eval_block(stmts, trailing);

        self.frames.pop();

        result
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

        let format_str = self.format_string_of(format_expr, span, "format")?;
        let result = self.render_format_args(&format_str, &args[1..], span)?;

        Ok(Value::Str(result.into()))
    }

    fn format_error(&mut self, message: String, span: miette::SourceSpan) -> Unwind<'arena> {
        self.errors.push(Error::FormatParseError {
            message: message.into(),
            src: self.current_src(),
            span,
        });
        Unwind::Break
    }

    fn render_format_args(
        &mut self,
        format: &str,
        values: &'arena [&'arena Expression<'arena>],
        span: miette::SourceSpan,
    ) -> Result<String, Unwind<'arena>> {
        let chunks = match parse_format_string(format) {
            Ok(chunks) => chunks,
            Err(err) => return Err(self.format_error(err.message(), span)),
        };

        let placeholders = chunks
            .iter()
            .filter(|chunk| matches!(chunk, FormatChunk::Arg(_)))
            .count();

        if placeholders != values.len() {
            return Err(self.format_error(
                format!(
                    "format string provides {placeholders} placeholders, but found {}",
                    values.len()
                ),
                span,
            ));
        }

        let mut result = String::new();
        let mut values_iter = values.iter();

        for chunk in chunks {
            match chunk {
                FormatChunk::Literal(text) => result.push_str(&text),
                FormatChunk::Arg(spec) => {
                    let expr = values_iter.next().expect("placeholder count was checked");
                    let value = self.eval_expr(expr)?;
                    result.push_str(&self.render_value(&value, spec, span)?);
                }
            }
        }

        Ok(result)
    }

    fn render_value(
        &mut self,
        value: &Value<'arena>,
        spec: FormatSpec,
        span: miette::SourceSpan,
    ) -> Result<String, Unwind<'arena>> {
        match spec {
            FormatSpec::Display => Ok(self.format_display(value)),
            FormatSpec::Debug => Ok(self.format_debug(value)),
            FormatSpec::Hex | FormatSpec::Oct | FormatSpec::Bin => match value {
                Value::Int(number) => {
                    let raw = *number as u64;
                    Ok(match spec {
                        FormatSpec::Hex => format!("{raw:x}"),
                        FormatSpec::Oct => format!("{raw:o}"),
                        _ => format!("{raw:b}"),
                    })
                }
                other => Err(self.format_error(
                    format!(
                        "format requires integer type, but found `{}`",
                        other.type_name()
                    ),
                    span,
                )),
            },
            FormatSpec::Float { precision } => match value {
                Value::Float(number) => Ok(format!("{:.*}", precision as usize, number)),
                other => Err(self.format_error(
                    format!(
                        "format requires float type, but found `{}`",
                        other.type_name()
                    ),
                    span,
                )),
            },
        }
    }

    fn format_display(&self, value: &Value<'arena>) -> String {
        match value {
            Value::Float(number) => format!("{number:.6}"),
            other => self.display_value(other),
        }
    }

    fn format_debug(&self, value: &Value<'arena>) -> String {
        match value {
            Value::Str(text) => format!("\"{text}\""),
            Value::Char(chr) => format!("'{chr}'"),
            Value::Array(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(|item| self.format_debug(item))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Value::Record(record) => format!(
                "{} {{ {} }}",
                record.kind,
                record
                    .fields
                    .iter()
                    .map(|(name, item)| format!("{name}: {}", Self::format_debug(self, item)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            other => self.format_display(other),
        }
    }

    fn print_prefix(&self) -> Option<String> {
        match self.frames.last().map(|frame| &frame.context) {
            Some(FrameContext::Block { file, line }) => Some(format!("[comptime][{file}:{line}]")),
            Some(FrameContext::Function { name, file, line }) => {
                let name = self.interned(name);
                Some(format!("[comptime {name}][{file}:{line}]"))
            }
            _ => None,
        }
    }

    fn eval_macro_println(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
        with_newline: bool,
    ) -> Result<(), Unwind<'arena>> {
        let macro_name = if with_newline { "println" } else { "print" };

        let format_expr = match args.first() {
            Some(expr) => expr,
            None => {
                return Err(self.format_error(
                    format!("@{macro_name} requires at least one argument (format string)"),
                    span,
                ));
            }
        };

        let format_str = self.format_string_of(format_expr, span, macro_name)?;
        let result = self.render_format_args(&format_str, &args[1..], span)?;
        let line = match self.print_prefix() {
            Some(prefix) => format!("{prefix} {result}"),
            None => result,
        };

        if with_newline {
            println!("{line}");
        } else {
            print!("{line}");
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
        let prefix = self
            .print_prefix()
            .unwrap_or_else(|| "[comptime dbg]".to_string());
        let value_str = self.format_debug(&value);

        println!("{prefix} {value_str}");
        Ok(value)
    }

    fn eval_macro_panic(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let message = if args.is_empty() {
            "comptime panic".to_string()
        } else {
            let format_str = self.format_string_of(args[0], span, "panic")?;
            self.render_format_args(&format_str, &args[1..], span)?
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
        let ty = self.type_arg(args.first(), span, "@zeen_type")?;

        Ok(Value::Type(ty))
    }

    fn eval_macro_fields(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let ty = self.type_arg(args.first(), span, "@fields")?;
        let decl = self.decl_for_type(ty, span)?;

        let fields = match &decl.kind {
            DeclarationKind::StructDecl { fields, .. } => *fields,
            DeclarationKind::EnumDecl { .. } => {
                self.errors.push(Error::TypeMismatch {
                    message: "@fields expects a struct type, use @variants for enums".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@fields expects a struct type".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        Ok(Value::Array(self.field_records(fields)))
    }

    fn eval_macro_variants(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let ty = self.type_arg(args.first(), span, "@variants")?;
        let decl = self.decl_for_type(ty, span)?;

        let variants = match &decl.kind {
            DeclarationKind::EnumDecl { variants, .. } => *variants,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@variants expects an enum type".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let mut items = Vec::with_capacity(variants.len());

        for (index, variant) in variants.iter().enumerate() {
            let mut fields = vec![
                ("index", Value::Int(index as i128)),
                ("name", Value::Str(self.interned(&variant.name).into())),
            ];

            match variant.payload {
                None => fields.push(("payload", Value::Str("none".into()))),
                Some(EnumVariantPayload::Single(ty)) => {
                    fields.push(("payload", Value::Str("value".into())));
                    fields.push(("type", Value::Type(ty)));
                }
                Some(EnumVariantPayload::Anonymous(payload_fields)) => {
                    fields.push(("payload", Value::Str("struct".into())));
                    fields.push(("fields", Value::Array(self.field_records(payload_fields))));
                }
            }

            items.push(Value::Record(Rc::new(Record {
                kind: "Variant",
                fields,
            })));
        }

        Ok(Value::Array(items))
    }

    fn field_records(&mut self, fields: &'arena [StructField<'arena>]) -> Vec<Value<'arena>> {
        let mut items = Vec::with_capacity(fields.len());

        for (index, field) in fields.iter().enumerate() {
            items.push(Value::Record(Rc::new(Record {
                kind: "Field",
                fields: vec![
                    ("index", Value::Int(index as i128)),
                    ("name", Value::Str(self.interned(&field.name).into())),
                    ("type", Value::Type(field.ty)),
                ],
            })));
        }

        items
    }

    fn eval_macro_type_info(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let ty = self.type_arg(args.first(), span, "@typeInfo")?;
        let decl = self.decl_for_type(ty, span)?;
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

        let ty = self.type_arg(args.first(), span, "@hasField")?;
        let field_name = self.string_arg(args[1], span, "@hasField")?;

        let decl = self.decl_for_type(ty, span)?;
        let has = match &decl.kind {
            DeclarationKind::StructDecl { fields, .. } => {
                fields.iter().any(|f| self.interned(&f.name) == field_name)
            }
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

        let ty = self.type_arg(args.first(), span, "@fieldType")?;
        let field_name = self.string_arg(args[1], span, "@fieldType")?;

        let decl = self.decl_for_type(ty, span)?;
        let field_ty = match &decl.kind {
            DeclarationKind::StructDecl { fields, .. } => fields
                .iter()
                .find(|f| self.interned(&f.name) == field_name)
                .map(|f| f.ty),
            _ => None,
        };

        match field_ty {
            Some(ty) => Ok(Value::Type(ty)),
            None => {
                self.errors.push(Error::TypeMismatch {
                    message: format!("field not found: {field_name}").into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    fn type_arg(
        &mut self,
        expr: Option<&&'arena Expression<'arena>>,
        span: miette::SourceSpan,
        macro_name: &str,
    ) -> Result<&'arena TypeExpr<'arena>, Unwind<'arena>> {
        let Some(expr) = expr else {
            self.errors.push(Error::TypeMismatch {
                message: format!("{macro_name} requires a type argument").into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        };

        match &expr.kind {
            ExpressionKind::Type(ty) => {
                if let TypeKind::Named {
                    name,
                    generic_args: None,
                } = &ty.kind
                    && let Some(Value::Type(bound)) = self.named_value(*name)
                {
                    return Ok(bound);
                }

                Ok(*ty)
            }
            _ => match self.eval_expr(expr)? {
                Value::Type(ty) => Ok(ty),
                other => {
                    self.errors.push(Error::TypeMismatch {
                        message: format!("{macro_name} expects a type, got {}", other.type_name())
                            .into(),
                        src: self.current_src(),
                        span,
                    });
                    Err(Unwind::Break)
                }
            },
        }
    }

    fn string_arg(
        &mut self,
        expr: &'arena Expression<'arena>,
        span: miette::SourceSpan,
        macro_name: &str,
    ) -> Result<String, Unwind<'arena>> {
        if let ExpressionKind::Literal(Literal::String(spur)) = &expr.kind {
            return Ok(self.interned(spur));
        }

        match self.eval_expr(expr)? {
            Value::Str(text) => Ok(text.to_string()),
            other => {
                self.errors.push(Error::TypeMismatch {
                    message: format!("{macro_name} expects a string, got {}", other.type_name())
                        .into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    fn named_value(&mut self, name: Spur) -> Option<Value<'arena>> {
        if let Some(value) = self.local_value(name) {
            return Some(value);
        }

        let module = self.current_module();

        if self.is_comptime_const(&module, name)
            && let Ok(value) = self.eval_const(&module, name)
        {
            return Some(value);
        }

        None
    }

    fn decl_for_type(
        &mut self,
        ty: &'arena TypeExpr<'arena>,
        span: miette::SourceSpan,
    ) -> Result<&'arena Declaration<'arena>, Unwind<'arena>> {
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
