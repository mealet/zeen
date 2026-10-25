use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use bumpalo::Bump;
use lasso::{Rodeo, Spur};

use miette::NamedSource;

use rustc_hash::{FxHashMap, FxHashSet};
use smol_str::SmolStr;

use zeen_ast::{
    Declaration, DeclarationKind, Expression, ExpressionKind, Statement, StatementKind, TypeExpr,
    declarations::{EnumVariantPayload, GenericType, StructField},
    expressions::{BinaryOp, Literal, UnaryOp},
    types::{BuiltinType, TypeKind},
};
use zeen_driver::Target;
use zeen_layout::{LayoutEngine, Shape};

use crate::error::Error;
use crate::format::{FormatChunk, FormatSpec, parse_format_string};
use crate::value::{StructValue, Value};

pub const STEP_LIMIT: usize = 200_000;
pub const DEPTH_LIMIT: usize = 200;
pub const EMIT_LIMIT: usize = 1024;
pub const LAYOUT_DEPTH_LIMIT: usize = 64;

pub type EvalFrame<'arena> = Vec<FxHashMap<Spur, Value<'arena>>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DeclNamespace {
    Value,
    Type,
}

pub(crate) type DeclNameKey = (String, DeclNamespace, Spur);
pub(crate) type DeclSite = (String, usize);

#[derive(Clone)]
pub(crate) struct CallOutcome<'arena> {
    pub(crate) value: Value<'arena>,
    pub(crate) receiver: Option<Value<'arena>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ValueKey {
    Int(i128),
    Float(u64),
    Bool(bool),
    Char(char),
    Str(Rc<str>),
    Array(Vec<ValueKey>),
    Struct(usize, PathBuf, Vec<(Rc<str>, ValueKey)>),
    Type(usize),
    Expr(usize),
    Decl(usize),
}

fn value_key(value: &Value<'_>) -> ValueKey {
    match value {
        Value::Int(value) => ValueKey::Int(*value),
        Value::Float(value) => ValueKey::Float(value.to_bits()),
        Value::Bool(value) => ValueKey::Bool(*value),
        Value::Char(value) => ValueKey::Char(*value),
        Value::Str(value) => ValueKey::Str(value.clone()),
        Value::Array(items) => ValueKey::Array(items.iter().map(value_key).collect()),
        Value::Struct(value) => ValueKey::Struct(
            value.decl as *const Declaration<'_> as usize,
            value.module.clone(),
            value
                .fields
                .iter()
                .map(|(name, item)| (name.clone(), value_key(item)))
                .collect(),
        ),
        Value::Type(ty) => ValueKey::Type(*ty as *const TypeExpr<'_> as usize),
        Value::Expr(expr) => ValueKey::Expr(*expr as *const Expression<'_> as usize),
        Value::Decl(decl) => ValueKey::Decl(*decl as *const Declaration<'_> as usize),
    }
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct FnCacheKey {
    decl: usize,
    module: PathBuf,
    args: Vec<ValueKey>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) struct ShapeCacheKey {
    module: PathBuf,
    ty: TypeKey,
    bindings: Vec<Vec<(Spur, PathBuf, TypeKey)>>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum TypeKey {
    Builtin(BuiltinType),
    SelfType,
    SelfAlias,
    VaArgs,
    Named(Spur, Option<Vec<TypeKey>>),
    Const(Box<TypeKey>),
    TypeOf(usize),
    SinglePointer(Box<TypeKey>),
    ManyPointer(Box<TypeKey>),
    Array(Box<TypeKey>, Option<usize>),
    Fn(Vec<TypeKey>, Box<TypeKey>),
    FatFn(Vec<TypeKey>, Box<TypeKey>, bool),
}

fn type_key(ty: &TypeExpr<'_>) -> TypeKey {
    match &ty.kind {
        TypeKind::Builtin(builtin) => TypeKey::Builtin(*builtin),
        TypeKind::SelfType => TypeKey::SelfType,
        TypeKind::SelfAlias => TypeKey::SelfAlias,
        TypeKind::VaArgs => TypeKey::VaArgs,
        TypeKind::Named { name, generic_args } => TypeKey::Named(
            *name,
            generic_args.map(|args| args.iter().map(|arg| type_key(arg)).collect()),
        ),
        TypeKind::Const(inner) => TypeKey::Const(Box::new(type_key(inner))),
        TypeKind::TypeOf(expr) => TypeKey::TypeOf(*expr as *const Expression<'_> as usize),
        TypeKind::SinglePointer(inner) => TypeKey::SinglePointer(Box::new(type_key(inner))),
        TypeKind::ManyPointer(inner) => TypeKey::ManyPointer(Box::new(type_key(inner))),
        TypeKind::Array { element, len } => TypeKey::Array(
            Box::new(type_key(element)),
            len.map(|len| len as *const Expression<'_> as usize),
        ),
        TypeKind::Fn { params, ret, .. } => TypeKey::Fn(
            params.iter().map(|param| type_key(param)).collect(),
            Box::new(type_key(ret)),
        ),
        TypeKind::FatFn { params, ret, once } => TypeKey::FatFn(
            params.iter().map(|param| type_key(param)).collect(),
            Box::new(type_key(ret)),
            *once,
        ),
    }
}

fn shape_cache_key(module: &Path, ty: &TypeExpr<'_>, bindings: &[ShapeFrame<'_>]) -> ShapeCacheKey {
    let bindings = bindings
        .iter()
        .map(|frame| {
            let mut entries: Vec<(Spur, PathBuf, TypeKey)> = frame
                .iter()
                .map(|(name, (module, ty))| (*name, module.clone(), type_key(ty)))
                .collect();
            entries.sort_by_key(|entry| entry.0);
            entries
        })
        .collect();

    ShapeCacheKey {
        module: module.to_path_buf(),
        ty: type_key(ty),
        bindings,
    }
}

pub type EvalResult<'arena> = Result<Value<'arena>, Unwind<'arena>>;
pub type StmtResult<'arena> = Result<Option<Value<'arena>>, Unwind<'arena>>;

type ShapeFrame<'arena> = FxHashMap<Spur, (PathBuf, &'arena TypeExpr<'arena>)>;
type ShapeBindings<'arena> = Vec<ShapeFrame<'arena>>;

#[derive(Debug)]
pub enum Unwind<'arena> {
    Return(Value<'arena>),
    Break,
    Continue,
}

pub struct ModuleScope<'arena> {
    pub(crate) src: NamedSource<Arc<String>>,
    pub(crate) consts: HashMap<Spur, ConstEntry<'arena>>,
    pub(crate) pub_consts: FxHashSet<Spur>,
    pub(crate) fns: HashMap<Spur, &'arena Declaration<'arena>>,
    pub(crate) structs: HashMap<Spur, &'arena Declaration<'arena>>,
    pub(crate) enums: HashMap<Spur, &'arena Declaration<'arena>>,
    pub(crate) aliases: HashMap<Spur, &'arena Declaration<'arena>>,
    pub(crate) comptime_structs: HashMap<Spur, &'arena Declaration<'arena>>,
}

pub enum ConstEntry<'arena> {
    Pending(&'arena Expression<'arena>, miette::SourceSpan),
    Evaluating(miette::SourceSpan),
    Done(Value<'arena>),
}

pub(crate) enum FrameContext {
    None,
    Block {
        src: NamedSource<Arc<String>>,
        span: miette::SourceSpan,
    },
    Function {
        name: Spur,
        src: NamedSource<Arc<String>>,
        span: miette::SourceSpan,
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
    pub(crate) visibility: HashMap<PathBuf, Vec<PathBuf>>,

    pub(crate) frames: Vec<CallFrame<'arena>>,

    pub(crate) default_src: Option<NamedSource<Arc<String>>>,

    pub(crate) shadow_stack: Vec<FxHashSet<Spur>>,

    pub(crate) steps: usize,
    pub(crate) depth: usize,

    pub(crate) splice_depth: usize,
    pub(crate) capture_depth: usize,
    pub(crate) layout_depth: usize,

    pub(crate) emitted: Vec<(PathBuf, &'arena Declaration<'arena>)>,
    pub(crate) declared_names: FxHashMap<DeclNameKey, DeclSite>,
    pub(crate) fresh_counter: usize,

    pub(crate) layout: LayoutEngine,
    pub(crate) fn_cache: FxHashMap<FnCacheKey, CallOutcome<'arena>>,
    pub(crate) shape_cache: FxHashMap<ShapeCacheKey, Shape>,
    pub(crate) impure: usize,

    #[cfg(test)]
    pub(crate) calls: usize,
    #[cfg(test)]
    pub(crate) shape_computations: usize,

    pub(crate) errors: Vec<Error>,
}

impl<'arena> Interp<'arena> {
    pub fn new(
        arena: &'arena Bump,
        interner: Rc<RefCell<Rodeo>>,
        visibility: HashMap<PathBuf, Vec<PathBuf>>,
        target: &Target,
    ) -> Self {
        Self {
            arena,
            interner,
            scopes: FxHashMap::default(),
            visibility,
            frames: Vec::new(),
            default_src: None,
            shadow_stack: Vec::new(),
            steps: 0,
            depth: 0,
            splice_depth: 0,
            capture_depth: 0,
            layout_depth: 0,
            emitted: Vec::new(),
            declared_names: FxHashMap::default(),
            fresh_counter: 0,
            layout: LayoutEngine::for_target(target),
            fn_cache: FxHashMap::default(),
            shape_cache: FxHashMap::default(),
            impure: 0,
            #[cfg(test)]
            calls: 0,
            #[cfg(test)]
            shape_computations: 0,
            errors: Vec::new(),
        }
    }

    pub fn evaluate(
        &mut self,
        program: &'arena [&'arena Declaration<'arena>],
        module_of: impl Fn(&Declaration<'_>) -> PathBuf,
    ) -> Result<&'arena [&'arena Declaration<'arena>], Vec<Error>> {
        for decl in program {
            if let Some((key, _, site)) = self.declared_key(decl) {
                self.declared_names.entry(key).or_insert(site);
            }
        }

        for decl in program {
            let path = module_of(decl);

            let scope = self
                .scopes
                .entry(path.clone())
                .or_insert_with(|| ModuleScope {
                    src: decl.source.src(),
                    consts: HashMap::default(),
                    pub_consts: FxHashSet::default(),
                    fns: HashMap::default(),
                    structs: HashMap::default(),
                    enums: HashMap::default(),
                    aliases: HashMap::default(),
                    comptime_structs: HashMap::default(),
                });

            match &decl.kind {
                DeclarationKind::ComptimeConst {
                    name,
                    value,
                    is_pub,
                } => {
                    if *is_pub {
                        scope.pub_consts.insert(name.0);
                    }
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
                DeclarationKind::Alias(alias) => {
                    scope.aliases.entry(alias.name.0).or_insert(decl);
                }
                DeclarationKind::ComptimeStruct { name, .. } => {
                    scope.comptime_structs.entry(name.0).or_insert(decl);
                }
                DeclarationKind::ComptimeBlock(block) => {
                    self.frames.push(CallFrame {
                        module: path.clone(),
                        src: decl.source.src(),
                        scopes: vec![FxHashMap::default()],
                        context: FrameContext::Block {
                            src: decl.source.src(),
                            span: decl.source.span,
                        },
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

        let mut kept: Vec<&'arena Declaration<'arena>> = substituted
            .into_iter()
            .filter(|decl| {
                !matches!(
                    decl.kind,
                    DeclarationKind::ComptimeConst { .. }
                        | DeclarationKind::ComptimeFn { .. }
                        | DeclarationKind::ComptimeBlock(_)
                        | DeclarationKind::ComptimeStruct { .. }
                )
            })
            .collect();

        let mut index = 0;
        while index < self.emitted.len() {
            let module = self.emitted[index].0.clone();
            let decl = self.emitted[index].1;
            index += 1;

            self.default_src = Some(decl.source.src());

            match self.substitute_decl(&module, decl) {
                Ok(Some(new_decl)) => kept.push(new_decl),
                Ok(None) => kept.push(decl),
                Err(errors) => return Err(errors),
            }
        }

        if !self.errors.is_empty() {
            return Err(self.errors.clone());
        }

        Ok(self.arena.alloc_slice_copy(&kept))
    }

    pub(crate) fn interned(&self, spur: &Spur) -> String {
        crate::display::interned(&self.interner, spur)
    }

    pub(crate) fn is_capture_macro(&self, name: Spur) -> bool {
        matches!(
            self.interner.borrow().resolve(&name),
            "zeen_expr" | "zeen_decl" | "zeen_type"
        )
    }

    pub(crate) fn type_display(&self, ty: &TypeExpr<'arena>) -> String {
        crate::display::type_display(&self.interner, ty)
    }

    pub(crate) fn display_value(&self, value: &Value<'arena>) -> String {
        match value {
            Value::Type(ty) => self.type_display(ty),
            Value::Array(items) => {
                let rendered: Vec<String> =
                    items.iter().map(|item| self.display_value(item)).collect();
                format!("[{}]", rendered.join(", "))
            }
            Value::Struct(value) => {
                let rendered: Vec<String> = value
                    .fields
                    .iter()
                    .map(|(name, item)| format!("{name}: {}", self.display_value(item)))
                    .collect();
                format!("{} {{ {} }}", value.name, rendered.join(", "))
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

        if self
            .scopes
            .get(module)
            .is_some_and(|scope| scope.consts.contains_key(&name))
        {
            return true;
        }

        self.visible_modules(module).iter().any(|target| {
            self.scopes
                .get(target)
                .is_some_and(|scope| scope.consts.contains_key(&name))
        })
    }

    pub(crate) fn is_comptime_fn(&mut self, module: &PathBuf, name: Spur) -> bool {
        self.top_shadow();

        if self.name_shadowed(name) {
            return false;
        }

        if self
            .scopes
            .get(module)
            .is_some_and(|scope| scope.fns.contains_key(&name))
        {
            return true;
        }

        self.visible_modules(module).iter().any(|target| {
            self.scopes
                .get(target)
                .is_some_and(|scope| scope.fns.contains_key(&name))
        })
    }

    fn visible_modules(&self, module: &PathBuf) -> &[PathBuf] {
        self.visibility
            .get(module)
            .map(|targets| targets.as_slice())
            .unwrap_or(&[])
    }

    fn fn_is_pub(decl: &Declaration<'_>) -> bool {
        matches!(&decl.kind, DeclarationKind::ComptimeFn { is_pub: true, .. })
    }

    fn type_decl_is_pub(decl: &Declaration<'_>) -> bool {
        match &decl.kind {
            DeclarationKind::StructDecl { is_pub, .. }
            | DeclarationKind::EnumDecl { is_pub, .. } => *is_pub,
            _ => false,
        }
    }

    fn comptime_struct_is_pub(decl: &Declaration<'_>) -> bool {
        matches!(
            &decl.kind,
            DeclarationKind::ComptimeStruct { is_pub: true, .. }
        )
    }

    fn alias_is_pub(decl: &Declaration<'_>) -> bool {
        matches!(decl.kind, DeclarationKind::Alias(alias) if alias.is_pub)
    }

    fn is_self_receiver(ty: &TypeExpr<'_>) -> bool {
        match &ty.kind {
            TypeKind::SelfType | TypeKind::SelfAlias => true,
            TypeKind::Const(inner)
            | TypeKind::SinglePointer(inner)
            | TypeKind::ManyPointer(inner) => Self::is_self_receiver(inner),
            _ => false,
        }
    }

    fn pick_owner(
        &mut self,
        name: Spur,
        span: miette::SourceSpan,
        public: Vec<PathBuf>,
        private: Vec<PathBuf>,
        kind: &str,
    ) -> Result<Option<PathBuf>, Unwind<'arena>> {
        match public.len() {
            0 => {
                if !private.is_empty() {
                    let label = self.interned(&name);
                    self.errors.push(Error::TypeMismatch {
                        message: format!(
                            "comptime {kind} `{label}` is not public in the module that defines it"
                        )
                        .into(),
                        src: self.current_src(),
                        span,
                    });
                    return Err(Unwind::Break);
                }

                Ok(None)
            }

            1 => Ok(public.into_iter().next()),

            _ => {
                let label = self.interned(&name);
                self.errors.push(Error::TypeMismatch {
                    message: format!("ambiguous comptime {kind} `{label}`").into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    pub(crate) fn resolve_comptime_const(
        &mut self,
        module: &PathBuf,
        name: Spur,
        span: miette::SourceSpan,
    ) -> Result<Option<PathBuf>, Unwind<'arena>> {
        if self
            .scopes
            .get(module)
            .is_some_and(|scope| scope.consts.contains_key(&name))
        {
            return Ok(Some(module.clone()));
        }

        let mut public = Vec::new();
        let mut private = Vec::new();

        for target in self.visible_modules(module).to_vec() {
            let Some(scope) = self.scopes.get(&target) else {
                continue;
            };

            if !scope.consts.contains_key(&name) {
                continue;
            }

            if scope.pub_consts.contains(&name) {
                public.push(target);
            } else {
                private.push(target);
            }
        }

        self.pick_owner(name, span, public, private, "constant")
    }

    pub(crate) fn resolve_comptime_fn(
        &mut self,
        module: &PathBuf,
        name: Spur,
        span: miette::SourceSpan,
    ) -> Result<Option<PathBuf>, Unwind<'arena>> {
        if self
            .scopes
            .get(module)
            .is_some_and(|scope| scope.fns.contains_key(&name))
        {
            return Ok(Some(module.clone()));
        }

        let mut public = Vec::new();
        let mut private = Vec::new();

        for target in self.visible_modules(module).to_vec() {
            let Some(decl) = self
                .scopes
                .get(&target)
                .and_then(|scope| scope.fns.get(&name))
            else {
                continue;
            };

            if Self::fn_is_pub(decl) {
                public.push(target);
            } else {
                private.push(target);
            }
        }

        self.pick_owner(name, span, public, private, "function")
    }

    pub(crate) fn resolve_comptime_type(
        &mut self,
        module: &PathBuf,
        name: Spur,
        span: miette::SourceSpan,
    ) -> Result<Option<PathBuf>, Unwind<'arena>> {
        if self.scopes.get(module).is_some_and(|scope| {
            scope.structs.contains_key(&name) || scope.enums.contains_key(&name)
        }) {
            return Ok(Some(module.clone()));
        }

        let mut public = Vec::new();
        let mut private = Vec::new();

        for target in self.visible_modules(module).to_vec() {
            let Some(decl) = self
                .scopes
                .get(&target)
                .and_then(|scope| scope.structs.get(&name).or_else(|| scope.enums.get(&name)))
            else {
                continue;
            };

            if Self::type_decl_is_pub(decl) {
                public.push(target);
            } else {
                private.push(target);
            }
        }

        self.pick_owner(name, span, public, private, "type")
    }

    pub(crate) fn resolve_comptime_struct(
        &mut self,
        module: &PathBuf,
        name: Spur,
        span: miette::SourceSpan,
    ) -> Result<Option<PathBuf>, Unwind<'arena>> {
        if self
            .scopes
            .get(module)
            .is_some_and(|scope| scope.comptime_structs.contains_key(&name))
        {
            return Ok(Some(module.clone()));
        }

        let mut public = Vec::new();
        let mut private = Vec::new();

        for target in self.visible_modules(module).to_vec() {
            let Some(decl) = self
                .scopes
                .get(&target)
                .and_then(|scope| scope.comptime_structs.get(&name))
            else {
                continue;
            };

            if Self::comptime_struct_is_pub(decl) {
                public.push(target);
            } else {
                private.push(target);
            }
        }

        self.pick_owner(name, span, public, private, "struct")
    }

    pub(crate) fn comptime_struct_decl(
        &self,
        owner: &PathBuf,
        name: Spur,
    ) -> Option<&'arena Declaration<'arena>> {
        self.scopes
            .get(owner)
            .and_then(|scope| scope.comptime_structs.get(&name))
            .copied()
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

        self.run_fn(fn_decl, fn_src, module.clone(), name, span, args)
            .map(|outcome| outcome.value)
    }

    fn run_fn(
        &mut self,
        fn_decl: &'arena Declaration<'arena>,
        fn_src: NamedSource<Arc<String>>,
        module: PathBuf,
        name: Spur,
        span: miette::SourceSpan,
        args: &[Value<'arena>],
    ) -> Result<CallOutcome<'arena>, Unwind<'arena>> {
        let key = FnCacheKey {
            decl: fn_decl as *const Declaration<'_> as usize,
            module: module.clone(),
            args: args.iter().map(value_key).collect(),
        };

        if let Some(outcome) = self.fn_cache.get(&key) {
            return Ok(outcome.clone());
        }

        let impure = self.impure;
        let outcome = self.run_fn_uncached(fn_decl, fn_src, module, name, span, args)?;

        if self.impure == impure {
            self.fn_cache.insert(key, outcome.clone());
        }

        Ok(outcome)
    }

    fn run_fn_uncached(
        &mut self,
        fn_decl: &'arena Declaration<'arena>,
        fn_src: NamedSource<Arc<String>>,
        module: PathBuf,
        name: Spur,
        span: miette::SourceSpan,
        args: &[Value<'arena>],
    ) -> Result<CallOutcome<'arena>, Unwind<'arena>> {
        #[cfg(test)]
        {
            self.calls += 1;
        }

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

        let (params, body, return_type) = match &fn_decl.kind {
            DeclarationKind::ComptimeFn {
                params,
                body,
                return_type,
                ..
            }
            | DeclarationKind::FnDecl {
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

        let self_name = params
            .iter()
            .find(|param| param.name.is_none() && Self::is_self_receiver(param.ty))
            .map(|_| self.interner.borrow_mut().get_or_intern("self"));

        let mut bindings: FxHashMap<Spur, Value<'arena>> = FxHashMap::default();
        for (param, value) in params.iter().zip(args.iter()) {
            match param.name {
                Some(param_name) => {
                    bindings.insert(param_name, value.clone());
                }
                None if self_name.is_some() => {
                    bindings.insert(self_name.expect("self binding"), value.clone());
                }
                None => {}
            }
        }

        let caller_src = self.current_src();

        self.frames.push(CallFrame {
            module,
            src: fn_src,
            scopes: vec![bindings],
            context: FrameContext::Function {
                name,
                src: caller_src,
                span,
            },
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

        let receiver = self_name.and_then(|name| {
            let frame = self.frames.last()?;
            frame.scopes.first()?.get(&name).cloned()
        });

        self.pop_shadow();
        self.depth -= 1;
        self.frames.pop();

        match result {
            Ok(Some(value)) => Ok(CallOutcome { value, receiver }),
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
                    Ok(CallOutcome {
                        value: Value::Bool(false),
                        receiver,
                    })
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
                let rhs = self.eval_expr(value)?;
                self.assign_place(object, rhs, stmt.span)?;
                Ok(None)
            }

            StatementKind::CompoundAssign { object, value, op } => {
                let rhs_value = self.eval_expr(value)?;
                let lhs_value = self.eval_expr(object)?;

                let computed = self.apply_binary(*op, lhs_value, rhs_value, stmt.span)?;
                self.assign_place(object, computed, stmt.span)?;
                Ok(None)
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

        let outcome = match &iterator.kind {
            ExpressionKind::Range {
                start,
                end,
                inclusive,
            } => {
                let start_value = match start {
                    Some(expr) => Some(self.eval_expr(expr)?),
                    None => None,
                };
                let end_value = match end {
                    Some(expr) => Some(self.eval_expr(expr)?),
                    None => None,
                };

                self.run_for_range(name, start_value, end_value, *inclusive, block, span)
            }

            _ => match self.eval_expr(iterator)? {
                Value::Int(total) if total >= 0 => {
                    self.run_for_range(name, None, Some(Value::Int(total)), false, block, span)
                }

                Value::Int(total) => {
                    self.errors.push(Error::TypeMismatch {
                        message: format!("comptime `for` count must not be negative, got {total}")
                            .into(),
                        src: self.current_src(),
                        span,
                    });
                    Err(Unwind::Break)
                }

                Value::Array(items) => self.run_for_iteration(name, items, block),

                Value::Str(text) => {
                    let chars: Vec<Value<'arena>> = text.chars().map(Value::Char).collect();
                    self.run_for_iteration(name, chars, block)
                }

                other => {
                    self.for_unsupported(other, span);
                    Err(Unwind::Break)
                }
            },
        };

        self.depth -= 1;

        match outcome {
            Ok(()) => Ok(None),
            Err(unwind) => Err(unwind),
        }
    }

    fn run_for_range(
        &mut self,
        name: Spur,
        start: Option<Value<'arena>>,
        end: Option<Value<'arena>>,
        inclusive: bool,
        block: &'arena Statement<'arena>,
        span: miette::SourceSpan,
    ) -> Result<(), Unwind<'arena>> {
        let start = match start {
            Some(Value::Int(value)) => value,
            Some(other) => {
                self.for_unsupported(other, span);
                return Err(Unwind::Break);
            }
            None => 0,
        };

        let end = match end {
            Some(Value::Int(value)) => value,
            Some(other) => {
                self.for_unsupported(other, span);
                return Err(Unwind::Break);
            }
            None => {
                self.errors.push(Error::TypeMismatch {
                    message: "comptime `for` requires an upper bound in a range expression".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let end = if inclusive { end + 1 } else { end };
        let values: Vec<Value<'arena>> = (start..end).map(Value::Int).collect();

        self.run_for_iteration(name, values, block)
    }

    fn for_unsupported(&mut self, value: Value<'arena>, span: miette::SourceSpan) {
        self.errors.push(Error::TypeMismatch {
            message: format!(
                "comptime `for` supports integer ranges, arrays and strings, got {}",
                value.type_name()
            )
            .into(),
            src: self.current_src(),
            span,
        });
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

        match self.resolve_comptime_const(&module, name, span) {
            Ok(Some(owner)) => return self.eval_const(&owner, name),
            Ok(None) => {}
            Err(unwind) => return Err(unwind),
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

    fn place_path(&self, expr: &Expression<'arena>) -> Option<(Spur, Vec<Rc<str>>)> {
        match &expr.kind {
            ExpressionKind::Ident { name, .. } => Some((*name, Vec::new())),
            ExpressionKind::FieldAccess { object, field } => {
                let ExpressionKind::Ident { name, .. } = &field.kind else {
                    return None;
                };

                let (root, mut path) = self.place_path(object)?;
                path.push(Rc::from(self.interned(name)));
                Some((root, path))
            }
            ExpressionKind::Unary {
                op: UnaryOp::Deref,
                expr,
            } => self.place_path(expr),
            _ => None,
        }
    }

    fn assign_place(
        &mut self,
        object: &Expression<'arena>,
        value: Value<'arena>,
        span: miette::SourceSpan,
    ) -> Result<(), Unwind<'arena>> {
        let Some((root, path)) = self.place_path(object) else {
            self.errors.push(Error::Unsupported {
                message: "only names and fields can be assigned in comptime"
                    .to_string()
                    .into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        };

        if path.is_empty() {
            return self.store_local(root, value, span);
        }

        let mut current = self.eval_ident_value(root, span)?;

        if !Self::set_field(&mut current, &path, value) {
            self.errors.push(Error::TypeMismatch {
                message: format!(
                    "cannot assign field `{}` on `{}`",
                    path.iter()
                        .map(|part| part.to_string())
                        .collect::<Vec<_>>()
                        .join("."),
                    current.type_name()
                )
                .into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        self.store_local(root, current, span)
    }

    fn set_field(value: &mut Value<'arena>, path: &[Rc<str>], new: Value<'arena>) -> bool {
        let Some((head, rest)) = path.split_first() else {
            *value = new;
            return true;
        };

        let Value::Struct(record) = value else {
            return false;
        };

        let record = Rc::make_mut(record);

        let Some((_, slot)) = record
            .fields
            .iter_mut()
            .find(|(name, _)| name.as_ref() == head.as_ref())
        else {
            return false;
        };

        Self::set_field(slot, rest, new)
    }

    fn store_local(
        &mut self,
        name: Spur,
        value: Value<'arena>,
        span: miette::SourceSpan,
    ) -> Result<(), Unwind<'arena>> {
        let frame = self.frames.last_mut().expect("comptime frame");

        if let Some(scope) = frame
            .scopes
            .iter_mut()
            .rev()
            .find(|scope| scope.contains_key(&name))
        {
            scope.insert(name, value);
            return Ok(());
        }

        let label = self.interned(&name);
        self.errors.push(Error::UnresolvedIdent {
            name: label.into(),
            src: self.current_src(),
            span,
        });
        Err(Unwind::Break)
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
        if let ExpressionKind::FieldAccess { object, field } = &callee.kind {
            return self.eval_method_call(object, field, args, span);
        }

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

        let owner = match self.resolve_comptime_fn(&module, name, span) {
            Ok(Some(owner)) => owner,
            Ok(None) => {
                let label = self.interned(&name);
                self.errors.push(Error::UnresolvedIdent {
                    name: label.into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
            Err(unwind) => return Err(unwind),
        };

        let mut values: Vec<Value<'arena>> = Vec::with_capacity(args.len());
        for arg in args.iter() {
            values.push(self.eval_expr(arg)?);
        }

        self.eval_comptime_call(&owner, name, span, &values)
    }

    fn eval_method_call(
        &mut self,
        object: &Expression<'arena>,
        field: &Expression<'arena>,
        args: &'arena [&'arena Expression<'arena>],
        span: miette::SourceSpan,
    ) -> EvalResult<'arena> {
        let receiver = self.eval_expr(object)?;

        let method_name = match &field.kind {
            ExpressionKind::Ident { name, .. } => *name,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "comptime method call expects a method name".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let value = match receiver {
            Value::Struct(value) => value,
            other => {
                self.errors.push(Error::TypeMismatch {
                    message: format!(
                        "method call expects a comptime struct, got {}",
                        other.type_name()
                    )
                    .into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let methods = match &value.decl.kind {
            DeclarationKind::ComptimeStruct { methods, .. } => *methods,
            _ => {
                self.errors.push(Error::Unsupported {
                    message: "expected comptime struct declaration".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let method = methods.iter().find(|method| {
            matches!(&method.kind, DeclarationKind::FnDecl { name, .. } if name.0 == method_name)
        });

        let method = match method {
            Some(method) => *method,
            None => {
                let label = self.interned(&method_name);
                self.errors.push(Error::TypeMismatch {
                    message: format!("`{}` has no method `{label}`", value.name).into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let fn_src = self.scope_src(&value.module);
        let mutates = Self::mutates_receiver(method);

        let mut values = Vec::with_capacity(args.len() + 1);
        values.push(Value::Struct(value.clone()));

        for arg in args.iter() {
            values.push(self.eval_expr(arg)?);
        }

        let outcome = self.run_fn(
            method,
            fn_src,
            value.module.clone(),
            method_name,
            span,
            &values,
        )?;

        if mutates && let Some(receiver) = outcome.receiver {
            self.assign_place(object, receiver, span)?;
        }

        Ok(outcome.value)
    }

    fn mutates_receiver(method: &Declaration<'_>) -> bool {
        let DeclarationKind::FnDecl { params, .. } = &method.kind else {
            return false;
        };

        params
            .iter()
            .find(|param| param.name.is_none() && Self::is_self_receiver(param.ty))
            .is_some_and(|param| match &param.ty.kind {
                TypeKind::SinglePointer(inner) => !matches!(&inner.kind, TypeKind::Const(_)),
                _ => false,
            })
    }

    fn eval_struct_init(
        &mut self,
        ty: &Expression<'arena>,
        fields: Option<&'arena [zeen_ast::expressions::FieldInit<'arena>]>,
        span: miette::SourceSpan,
    ) -> EvalResult<'arena> {
        let name = match &ty.kind {
            ExpressionKind::Ident {
                name,
                generic_args: None,
            } => *name,
            _ => {
                self.errors.push(Error::Unsupported {
                    message: "comptime struct construction expects a struct name".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let module = self.current_module();

        let owner = match self.resolve_comptime_struct(&module, name, span) {
            Ok(Some(owner)) => owner,
            Ok(None) => {
                let label = self.interned(&name);
                self.errors.push(Error::UnresolvedIdent {
                    name: label.into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
            Err(unwind) => return Err(unwind),
        };

        let decl = match self.comptime_struct_decl(&owner, name) {
            Some(decl) => decl,
            None => {
                let label = self.interned(&name);
                self.errors.push(Error::UnresolvedIdent {
                    name: label.into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let struct_fields = match &decl.kind {
            DeclarationKind::ComptimeStruct { fields, .. } => *fields,
            _ => {
                self.errors.push(Error::Unsupported {
                    message: "expected comptime struct declaration".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let mut provided: Vec<(Spur, Value<'arena>)> = Vec::new();

        if let Some(inits) = fields {
            for init in inits.iter() {
                if provided.iter().any(|(field, _)| *field == init.name) {
                    let label = self.interned(&init.name);
                    self.errors.push(Error::TypeMismatch {
                        message: format!("duplicate field `{label}` in comptime struct value")
                            .into(),
                        src: self.current_src(),
                        span: init.span,
                    });
                    return Err(Unwind::Break);
                }

                if !struct_fields.iter().any(|field| field.name == init.name) {
                    let label = self.interned(&init.name);
                    self.errors.push(Error::TypeMismatch {
                        message: format!("`{}` has no field `{label}`", self.interned(&name))
                            .into(),
                        src: self.current_src(),
                        span: init.span,
                    });
                    return Err(Unwind::Break);
                }

                let value = self.eval_expr(init.value)?;
                provided.push((init.name, value));
            }
        }

        let mut values = Vec::with_capacity(struct_fields.len());

        for field in struct_fields.iter() {
            let Some((_, value)) = provided.iter().find(|(name, _)| *name == field.name) else {
                let label = self.interned(&field.name);
                self.errors.push(Error::TypeMismatch {
                    message: format!(
                        "missing field `{label}` in comptime struct value `{}`",
                        self.interned(&name)
                    )
                    .into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            };

            values.push((Rc::from(self.interned(&field.name)), value.clone()));
        }

        Ok(Value::Struct(Rc::new(StructValue {
            decl,
            module: owner,
            name: self.interned(&name).into(),
            fields: values,
        })))
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

            ExpressionKind::StructInit { ty, fields } => {
                self.eval_struct_init(ty, *fields, expr.span)
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
                    Value::Struct(value) => match value.get(&field_name) {
                        Some(field) => Ok(field.clone()),
                        None => {
                            self.errors.push(Error::TypeMismatch {
                                message: format!("`{}` has no field `{field_name}`", value.name)
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
                                "field access expects a comptime struct, got {}",
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
                        self.impure += 1;
                        self.eval_macro_println(name.1, args, false)?;
                        Ok(Value::Bool(false))
                    }
                    "println" => {
                        self.impure += 1;
                        self.eval_macro_println(name.1, args, true)?;
                        Ok(Value::Bool(false))
                    }
                    "dbg" => {
                        self.impure += 1;
                        self.eval_macro_dbg(name.1, args)
                    }
                    "panic" => self.eval_macro_panic(name.1, args),
                    "zeen_type" => self.eval_macro_zeen_type(name.1, args),
                    "zeen_expr" => self.eval_macro_zeen_expr(name.1, args),
                    "zeen_decl" => self.eval_macro_zeen_decl(name.1, args),
                    "emit" => {
                        self.impure += 1;
                        self.eval_macro_emit(name.1, args)
                    }
                    "fresh_name" => {
                        self.impure += 1;
                        self.eval_macro_fresh_name(name.1, args)
                    }
                    "fields" => self.eval_macro_fields(name.1, args),
                    "variants" => self.eval_macro_variants(name.1, args),
                    "sizeof" => self.eval_macro_sizeof(name.1, args, false),
                    "alignof" => self.eval_macro_sizeof(name.1, args, true),
                    "typeInfo" => self.eval_macro_type_info(name.1, args),
                    "hasField" => self.eval_macro_has_field(name.1, args),
                    "fieldType" => self.eval_macro_field_type(name.1, args),
                    "uninit" | "void" => {
                        self.errors.push(Error::RuntimeOnly {
                            message: format!("`@{macro_name_owned}` is not available in comptime")
                                .into(),
                            src: self.current_src(),
                            span: expr.span,
                        });
                        Err(Unwind::Break)
                    }
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

    pub(crate) fn eval_interpolation_fragment(
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

            (UnaryOp::Deref, value @ Value::Struct(_)) => Ok(value),

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

        self.frames.push(CallFrame {
            module: self.current_module(),
            src: src.clone(),
            scopes: vec![FxHashMap::default()],
            context: FrameContext::Block { src, span },
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
            Value::Struct(value) => format!(
                "{} {{ {} }}",
                value.name,
                value
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
            Some(FrameContext::Block { src, span }) => {
                let (file, line) = source_site(src, *span);
                Some(format!("[comptime][{file}:{line}]"))
            }
            Some(FrameContext::Function { name, src, span }) => {
                let (file, line) = source_site(src, *span);
                let name = self.interned(name);
                Some(format!("[comptime {name}][{file}:{line}]"))
            }
            _ => None,
        }
    }

    fn call_trace(&self) -> Vec<crate::error::Trace> {
        self.frames
            .iter()
            .rev()
            .filter_map(|frame| match &frame.context {
                FrameContext::Function { src, span, .. } => Some(crate::error::Trace {
                    src: src.clone(),
                    span: *span,
                }),
                _ => None,
            })
            .collect()
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
            trace: self.call_trace(),
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
        let module = self.current_module();

        match self.capture_type(&module, ty) {
            Ok(captured) => Ok(Value::Type(captured)),
            Err(()) => Err(Unwind::Break),
        }
    }

    fn eval_macro_zeen_expr(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let Some(arg) = args.first() else {
            self.errors.push(Error::TypeMismatch {
                message: "@zeen_expr requires an expression".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        };

        if args.len() > 1 {
            self.errors.push(Error::TypeMismatch {
                message: "@zeen_expr takes exactly one expression".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let module = self.current_module();

        match self.capture_expr(&module, arg) {
            Ok(captured) => Ok(Value::Expr(captured)),
            Err(()) => Err(Unwind::Break),
        }
    }

    fn eval_macro_zeen_decl(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let decl = match args.first().map(|arg| &arg.kind) {
            Some(ExpressionKind::Decl(decl)) => *decl,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "@zeen_decl requires a declaration".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        if args.len() > 1 {
            self.errors.push(Error::TypeMismatch {
                message: "@zeen_decl takes exactly one declaration".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let module = self.current_module();

        match self.capture_decl(&module, decl) {
            Ok(captured) => Ok(Value::Decl(captured)),
            Err(()) => Err(Unwind::Break),
        }
    }

    fn declared_key(
        &self,
        decl: &'arena Declaration<'arena>,
    ) -> Option<(DeclNameKey, miette::SourceSpan, DeclSite)> {
        let (name, namespace) = match &decl.kind {
            DeclarationKind::FnDecl { name, .. } => (*name, DeclNamespace::Value),
            DeclarationKind::StructDecl { name, .. } => (*name, DeclNamespace::Type),
            DeclarationKind::InterfaceDecl { name, .. } => (*name, DeclNamespace::Type),
            DeclarationKind::EnumDecl { name, .. } => (*name, DeclNamespace::Type),
            DeclarationKind::Alias(alias) => (alias.name, DeclNamespace::Type),
            DeclarationKind::ExternVar { name, .. } => (*name, DeclNamespace::Value),
            DeclarationKind::GlobalVar { name, .. } => (*name, DeclNamespace::Value),
            _ => return None,
        };

        let src = decl.source.src();
        let site = source_site(&src, name.1);

        Some(((site.0.clone(), namespace, name.0), name.1, site))
    }

    fn eval_macro_emit(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        let Some(arg) = args.first() else {
            self.errors.push(Error::TypeMismatch {
                message: "@emit requires a zeen_decl argument".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        };

        if args.len() > 1 {
            self.errors.push(Error::TypeMismatch {
                message: "@emit takes exactly one zeen_decl argument".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let decl = match self.eval_expr(arg) {
            Ok(Value::Decl(decl)) => decl,
            Ok(other) => {
                self.errors.push(Error::TypeMismatch {
                    message: format!("@emit expects a zeen_decl, got {}", other.type_name()).into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
            Err(unwind) => return Err(unwind),
        };

        let unsupported = match &decl.kind {
            DeclarationKind::ComptimeConst { .. } => Some("comptime constant"),
            DeclarationKind::ComptimeFn { .. } => Some("comptime function"),
            DeclarationKind::ComptimeBlock(_) => Some("comptime block"),
            DeclarationKind::ComptimeStruct { .. } => Some("comptime struct"),
            DeclarationKind::Use { .. } => Some("use"),
            DeclarationKind::ExternLink { .. } => Some("extern link"),
            DeclarationKind::ExternInclude { .. } => Some("extern include"),
            DeclarationKind::ConditionalBlock(_) => Some("conditional block"),
            _ => None,
        };

        if let Some(what) = unsupported {
            self.errors.push(Error::Unsupported {
                message: format!("@emit cannot emit a {what} declaration").into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        if self.emitted.len() >= EMIT_LIMIT {
            self.errors.push(Error::LimitExceeded {
                message: format!("@emit exceeded the limit of {EMIT_LIMIT} declarations").into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        if let Some((key, name_span, site)) = self.declared_key(decl) {
            match self.declared_names.get(&key) {
                Some((file, line)) => {
                    let name = self.interned(&key.2);
                    self.errors.push(Error::DuplicateDefinition {
                        message: format!(
                            "duplicate definition of `{name}`, already defined at {file}:{line}"
                        )
                        .into(),
                        src: decl.source.src(),
                        span: name_span,
                    });
                    return Err(Unwind::Break);
                }
                None => {
                    self.declared_names.insert(key, site);
                }
            }
        }

        self.emitted.push((self.current_module(), decl));

        Ok(Value::Bool(false))
    }

    fn eval_macro_fresh_name(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
    ) -> EvalResult<'arena> {
        if args.len() != 1 {
            self.errors.push(Error::TypeMismatch {
                message: "@fresh_name takes exactly one prefix string".into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let prefix = self.string_arg(args[0], span, "@fresh_name")?;

        let counter = self.fresh_counter;
        self.fresh_counter += 1;

        Ok(Value::Str(format!("{prefix}_{counter}").into()))
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

        Ok(Value::Array(self.field_records(fields, span)?))
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

        let (variant_decl, variant_module) = self.reflection_struct("Variant", span)?;

        let mut items = Vec::with_capacity(variants.len());

        for (index, variant) in variants.iter().enumerate() {
            let (payload, ty, payload_fields) = match variant.payload {
                None => (
                    Value::Str("none".into()),
                    Value::Type(self.void_type(span)),
                    Value::Array(Vec::new()),
                ),
                Some(EnumVariantPayload::Single(ty)) => (
                    Value::Str("value".into()),
                    Value::Type(ty),
                    Value::Array(Vec::new()),
                ),
                Some(EnumVariantPayload::Anonymous(payload_fields)) => (
                    Value::Str("struct".into()),
                    Value::Type(self.void_type(span)),
                    Value::Array(self.field_records(payload_fields, span)?),
                ),
            };

            let value = self.make_struct_value(
                variant_decl,
                variant_module.clone(),
                vec![
                    ("index", Value::Int(index as i128)),
                    ("name", Value::Str(self.interned(&variant.name).into())),
                    ("payload", payload),
                    ("ty", ty),
                    ("fields", payload_fields),
                ],
                span,
            )?;

            items.push(value);
        }

        Ok(Value::Array(items))
    }

    fn field_records(
        &mut self,
        fields: &'arena [StructField<'arena>],
        span: miette::SourceSpan,
    ) -> Result<Vec<Value<'arena>>, Unwind<'arena>> {
        let (decl, module) = self.reflection_struct("Field", span)?;

        let mut items = Vec::with_capacity(fields.len());

        for (index, field) in fields.iter().enumerate() {
            let value = self.make_struct_value(
                decl,
                module.clone(),
                vec![
                    ("index", Value::Int(index as i128)),
                    ("name", Value::Str(self.interned(&field.name).into())),
                    ("ty", Value::Type(field.ty)),
                ],
                span,
            )?;

            items.push(value);
        }

        Ok(items)
    }

    fn reflection_struct(
        &mut self,
        kind: &str,
        span: miette::SourceSpan,
    ) -> Result<(&'arena Declaration<'arena>, PathBuf), Unwind<'arena>> {
        let module = self.current_module();
        let name = self.interner.borrow_mut().get_or_intern(kind);

        let owner = match self.resolve_comptime_struct(&module, name, span) {
            Ok(Some(owner)) => owner,
            Ok(None) => {
                self.errors.push(Error::Unsupported {
                    message: format!("comptime struct `{kind}` is not declared in core").into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
            Err(unwind) => return Err(unwind),
        };

        match self.comptime_struct_decl(&owner, name) {
            Some(decl) => Ok((decl, owner)),
            None => {
                self.errors.push(Error::Unsupported {
                    message: format!("comptime struct `{kind}` is not declared in core").into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    fn make_struct_value(
        &mut self,
        decl: &'arena Declaration<'arena>,
        module: PathBuf,
        values: Vec<(&'static str, Value<'arena>)>,
        span: miette::SourceSpan,
    ) -> Result<Value<'arena>, Unwind<'arena>> {
        let (name, fields) = match &decl.kind {
            DeclarationKind::ComptimeStruct { name, fields, .. } => (*name, *fields),
            _ => {
                self.errors.push(Error::Unsupported {
                    message: "expected comptime struct declaration".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
        };

        let mut out = Vec::with_capacity(fields.len());

        for field in fields.iter() {
            let field_name = self.interned(&field.name);

            let Some((_, value)) = values.iter().find(|(name, _)| *name == field_name) else {
                self.errors.push(Error::Unsupported {
                    message: format!("missing field `{field_name}` in comptime value").into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            };

            out.push((Rc::from(field_name), value.clone()));
        }

        Ok(Value::Struct(Rc::new(StructValue {
            decl,
            module,
            name: self.interned(&name.0).into(),
            fields: out,
        })))
    }

    fn void_type(&self, span: miette::SourceSpan) -> &'arena TypeExpr<'arena> {
        self.arena.alloc(TypeExpr {
            kind: TypeKind::Builtin(BuiltinType::void),
            span,
        })
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
            DeclarationKind::ComptimeStruct { .. } => "comptime_struct",
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

    fn eval_macro_sizeof(
        &mut self,
        span: miette::SourceSpan,
        args: &'arena [&'arena Expression<'arena>],
        align: bool,
    ) -> EvalResult<'arena> {
        let macro_name = if align { "@alignof" } else { "@sizeof" };

        if args.len() != 1 {
            self.errors.push(Error::TypeMismatch {
                message: format!("{macro_name} requires exactly one type argument").into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let ty = self.type_arg(args.first(), span, macro_name)?;
        let module = self.current_module();
        let bindings = ShapeBindings::default();
        let shape = self.shape_for_type(&module, ty, span, macro_name, &bindings)?;
        let layout = self.layout.layout(&shape);

        let value = if align { layout.align } else { layout.size };

        Ok(Value::Int(value as i128))
    }

    fn shape_for_type(
        &mut self,
        module: &PathBuf,
        ty: &'arena TypeExpr<'arena>,
        span: miette::SourceSpan,
        macro_name: &str,
        bindings: &[ShapeFrame<'arena>],
    ) -> Result<Shape, Unwind<'arena>> {
        let key = shape_cache_key(module, ty, bindings);

        if let Some(shape) = self.shape_cache.get(&key) {
            return Ok(shape.clone());
        }

        if self.layout_depth >= LAYOUT_DEPTH_LIMIT {
            self.errors.push(Error::LimitExceeded {
                message: format!("{macro_name} nested type depth limit exceeded").into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let impure = self.impure;

        #[cfg(test)]
        {
            self.shape_computations += 1;
        }

        self.layout_depth += 1;
        let shape = self.shape_for_type_inner(module, ty, span, macro_name, bindings);
        self.layout_depth -= 1;

        if let Ok(shape) = &shape
            && self.impure == impure
        {
            self.shape_cache.insert(key, shape.clone());
        }

        shape
    }

    fn shape_for_type_inner(
        &mut self,
        module: &PathBuf,
        ty: &'arena TypeExpr<'arena>,
        span: miette::SourceSpan,
        macro_name: &str,
        bindings: &[ShapeFrame<'arena>],
    ) -> Result<Shape, Unwind<'arena>> {
        match &ty.kind {
            TypeKind::Builtin(builtin) => Ok(builtin_shape(*builtin)),

            TypeKind::Const(inner) => {
                self.shape_for_type(module, inner, span, macro_name, bindings)
            }

            TypeKind::SinglePointer(_) | TypeKind::ManyPointer(_) | TypeKind::Fn { .. } => {
                Ok(Shape::Pointer)
            }

            TypeKind::FatFn { .. } => Ok(Shape::fat_fn()),

            TypeKind::Array { element, len } => match len {
                Some(len) => {
                    let count = self.array_len(len, span, macro_name)?;
                    let element_shape =
                        self.shape_for_type(module, element, span, macro_name, bindings)?;

                    Ok(Shape::Array(Box::new(element_shape), count))
                }
                None => {
                    self.shape_for_type(module, element, span, macro_name, bindings)?;

                    Ok(Shape::slice())
                }
            },

            TypeKind::Named { name, generic_args } => {
                if generic_args.is_none()
                    && let Some((frame, bound_module, bound_ty)) =
                        shape_binding_lookup(bindings, name)
                {
                    return self.shape_for_type(
                        &bound_module,
                        bound_ty,
                        span,
                        macro_name,
                        &bindings[..frame],
                    );
                }

                let label = self.interned(name);

                if let Some(owner) = self.resolve_comptime_alias(module, *name, span)? {
                    let decl = *self.scopes[&owner].aliases.get(name).unwrap();
                    let DeclarationKind::Alias(alias) = &decl.kind else {
                        unreachable!("alias scope holds a non-alias declaration")
                    };

                    let bound = self.shape_bindings(
                        alias.generics,
                        *name,
                        *generic_args,
                        module,
                        bindings,
                        span,
                        macro_name,
                    )?;

                    return self.shape_for_type(&owner, alias.ty, span, macro_name, &bound);
                }

                let owner = match self.resolve_comptime_type(module, *name, span)? {
                    Some(owner) => owner,
                    None => {
                        if self.resolve_comptime_struct(module, *name, span)?.is_some() {
                            self.errors.push(Error::Unsupported {
                                message: format!(
                                    "{macro_name} cannot be applied to comptime struct {label}"
                                )
                                .into(),
                                src: self.current_src(),
                                span,
                            });
                            return Err(Unwind::Break);
                        }

                        self.errors.push(Error::UnresolvedIdent {
                            name: label.into(),
                            src: self.current_src(),
                            span,
                        });
                        return Err(Unwind::Break);
                    }
                };

                let decl = *self.scopes[&owner]
                    .structs
                    .get(name)
                    .or_else(|| self.scopes[&owner].enums.get(name))
                    .expect("resolved type is a struct or enum");

                let generics = match &decl.kind {
                    DeclarationKind::StructDecl { generics, .. }
                    | DeclarationKind::EnumDecl { generics, .. } => *generics,
                    _ => None,
                };

                let bound = self.shape_bindings(
                    generics,
                    *name,
                    *generic_args,
                    module,
                    bindings,
                    span,
                    macro_name,
                )?;

                self.shape_for_decl(&owner, decl, span, macro_name, &bound)
            }

            _ => {
                self.errors.push(Error::Unsupported {
                    message: format!("{macro_name} does not support this type").into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn shape_bindings(
        &mut self,
        generics: Option<&'arena [GenericType<'arena>]>,
        name: Spur,
        generic_args: Option<&'arena [&'arena TypeExpr<'arena>]>,
        arg_module: &Path,
        parent: &[ShapeFrame<'arena>],
        span: miette::SourceSpan,
        macro_name: &str,
    ) -> Result<ShapeBindings<'arena>, Unwind<'arena>> {
        let generics = generics.unwrap_or(&[]);

        let Some(args) = generic_args else {
            if generics.is_empty() {
                return Ok(parent.to_vec());
            }

            let label = self.interned(&name);

            self.errors.push(Error::TypeMismatch {
                message: format!("{macro_name} requires generic arguments for {label}").into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        };

        if args.len() != generics.len() {
            let label = self.interned(&name);

            self.errors.push(Error::TypeMismatch {
                message: format!(
                    "{macro_name} expects {} generic arguments for {label}, got {}",
                    generics.len(),
                    args.len()
                )
                .into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        }

        let mut bindings = parent.to_vec();
        let mut frame: FxHashMap<Spur, (PathBuf, &'arena TypeExpr<'arena>)> = FxHashMap::default();

        for (generic, arg) in generics.iter().zip(args.iter()) {
            frame.insert(generic.name.0, (arg_module.to_path_buf(), *arg));
        }

        bindings.push(frame);

        Ok(bindings)
    }

    fn shape_for_decl(
        &mut self,
        module: &PathBuf,
        decl: &'arena Declaration<'arena>,
        span: miette::SourceSpan,
        macro_name: &str,
        bindings: &[ShapeFrame<'arena>],
    ) -> Result<Shape, Unwind<'arena>> {
        match &decl.kind {
            DeclarationKind::StructDecl { fields, .. } => {
                let mut shapes = Vec::with_capacity(fields.len());

                for field in fields.iter() {
                    shapes.push(self.shape_for_type(module, field.ty, span, macro_name, bindings)?);
                }

                Ok(Shape::Struct(shapes))
            }

            DeclarationKind::EnumDecl { variants, .. } => {
                let mut shapes = Vec::with_capacity(variants.len());

                for variant in variants.iter() {
                    let shape = match &variant.payload {
                        None => None,
                        Some(EnumVariantPayload::Single(ty)) => {
                            Some(self.shape_for_type(module, ty, span, macro_name, bindings)?)
                        }
                        Some(EnumVariantPayload::Anonymous(fields)) => {
                            let mut payload = Vec::with_capacity(fields.len());

                            for field in fields.iter() {
                                payload.push(self.shape_for_type(
                                    module, field.ty, span, macro_name, bindings,
                                )?);
                            }

                            Some(Shape::Struct(payload))
                        }
                    };

                    shapes.push(shape);
                }

                Ok(Shape::Enum(shapes))
            }

            _ => {
                self.errors.push(Error::Unsupported {
                    message: format!("{macro_name} does not support this type").into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    fn array_len(
        &mut self,
        len: &'arena Expression<'arena>,
        span: miette::SourceSpan,
        macro_name: &str,
    ) -> Result<u64, Unwind<'arena>> {
        let value = self.eval_expr(len)?;

        match value {
            Value::Int(count) if count > 0 => Ok(count as u64),
            Value::Int(count) => {
                self.errors.push(Error::TypeMismatch {
                    message: format!("{macro_name} requires a positive array length, got {count}")
                        .into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
            other => {
                self.errors.push(Error::TypeMismatch {
                    message: format!(
                        "{macro_name} requires a comptime_int array length, got {}",
                        other.type_name()
                    )
                    .into(),
                    src: self.current_src(),
                    span,
                });
                Err(Unwind::Break)
            }
        }
    }

    fn resolve_comptime_alias(
        &mut self,
        module: &PathBuf,
        name: Spur,
        span: miette::SourceSpan,
    ) -> Result<Option<PathBuf>, Unwind<'arena>> {
        if self
            .scopes
            .get(module)
            .is_some_and(|scope| scope.aliases.contains_key(&name))
        {
            return Ok(Some(module.clone()));
        }

        let mut public = Vec::new();
        let mut private = Vec::new();

        for target in self.visible_modules(module).to_vec() {
            let Some(decl) = self
                .scopes
                .get(&target)
                .and_then(|scope| scope.aliases.get(&name))
            else {
                continue;
            };

            if Self::alias_is_pub(decl) {
                public.push(target);
            } else {
                private.push(target);
            }
        }

        self.pick_owner(name, span, public, private, "type")
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
                    && let Some(Value::Type(bound)) = self.named_value(*name, span)
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

    fn named_value(&mut self, name: Spur, span: miette::SourceSpan) -> Option<Value<'arena>> {
        if let Some(value) = self.local_value(name) {
            return Some(value);
        }

        let module = self.current_module();

        match self.resolve_comptime_const(&module, name, span) {
            Ok(Some(owner)) => self.eval_const(&owner, name).ok(),
            _ => None,
        }
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

        let owner = match self.resolve_comptime_type(&module, name, span) {
            Ok(Some(owner)) => owner,
            Ok(None) => {
                let label = self.interned(&name);
                self.errors.push(Error::UnresolvedIdent {
                    name: label.into(),
                    src: self.current_src(),
                    span,
                });
                return Err(Unwind::Break);
            }
            Err(unwind) => return Err(unwind),
        };

        let Some(decl) = self.scopes.get(&owner).and_then(|scope| {
            scope
                .structs
                .get(&name)
                .copied()
                .or_else(|| scope.enums.get(&name).copied())
        }) else {
            let label = self.interned(&name);
            self.errors.push(Error::UnresolvedIdent {
                name: label.into(),
                src: self.current_src(),
                span,
            });
            return Err(Unwind::Break);
        };

        Ok(decl)
    }
}

fn shape_binding_lookup<'arena>(
    bindings: &[ShapeFrame<'arena>],
    name: &Spur,
) -> Option<(usize, PathBuf, &'arena TypeExpr<'arena>)> {
    for (frame, entries) in bindings.iter().enumerate().rev() {
        if let Some((module, ty)) = entries.get(name) {
            return Some((frame, module.clone(), *ty));
        }
    }

    None
}

fn builtin_shape(builtin: BuiltinType) -> Shape {
    match builtin {
        BuiltinType::i8 | BuiltinType::u8 => Shape::I8,
        BuiltinType::i16 | BuiltinType::u16 => Shape::I16,
        BuiltinType::i32 | BuiltinType::u32 => Shape::I32,
        BuiltinType::i64 | BuiltinType::u64 => Shape::I64,
        BuiltinType::isize | BuiltinType::usize => Shape::Isize,
        BuiltinType::f32 => Shape::F32,
        BuiltinType::f64 => Shape::F64,
        BuiltinType::bool => Shape::Bool,
        BuiltinType::char => Shape::Char,
        BuiltinType::void => Shape::Void,
        BuiltinType::never => Shape::Never,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Stats {
        calls: usize,
        shapes: usize,
        failed: bool,
    }

    fn interpret(src: &str) -> Stats {
        let rodeo = Rc::new(RefCell::new(Rodeo::default()));
        let bump = Bump::default();
        let content = Arc::new(src.to_string());
        let filename = Rc::new("test.zn".to_string());

        let mut tokens = zeen_lexer::tokenize(&content);
        let mut parser = zeen_parser::Parser::new(
            Rc::clone(&filename),
            Arc::clone(&content),
            &mut tokens,
            &bump,
            Rc::clone(&rodeo),
        );

        let program = parser.parse_program().expect("test program parses");
        let mut interp = Interp::new(
            &bump,
            Rc::clone(&rodeo),
            HashMap::default(),
            &Target::host(),
        );
        let failed = interp
            .evaluate(program, |_| PathBuf::from("test.zn"))
            .is_err();

        Stats {
            calls: interp.calls,
            shapes: interp.shape_computations,
            failed,
        }
    }

    #[test]
    fn caches_pure_function_calls() {
        let stats = interpret(
            r#"comptime fn double(n: comptime_int) comptime_int {
  return n * 2;
}

fn main() {
  comptime {
    let a = double(4);
    let b = double(4);
    let c = double(4);
  };
}
"#,
        );

        assert!(!stats.failed);
        assert_eq!(stats.calls, 1);
    }

    #[test]
    fn caches_function_calls_by_argument_values() {
        let stats = interpret(
            r#"comptime fn double(n: comptime_int) comptime_int {
  return n * 2;
}

fn main() {
  comptime {
    let a = double(1);
    let b = double(2);
    let c = double(1);
  };
}
"#,
        );

        assert!(!stats.failed);
        assert_eq!(stats.calls, 2);
    }

    #[test]
    fn does_not_cache_calls_with_side_effects() {
        let stats = interpret(
            r#"comptime fn tag() comptime_str {
  return @fresh_name("tag");
}

fn main() {
  comptime {
    let a = tag();
    let b = tag();
  };
}
"#,
        );

        assert!(!stats.failed);
        assert_eq!(stats.calls, 2);
    }

    #[test]
    fn reuses_sizeof_shapes() {
        let single = interpret(
            r#"struct Point {
  x: i32,
  y: i32,
}

fn main() {
  comptime {
    let a = @sizeof(Point);
  };
}
"#,
        );

        let repeated = interpret(
            r#"struct Point {
  x: i32,
  y: i32,
}

fn main() {
  comptime {
    let a = @sizeof(Point);
    let b = @sizeof(Point);
    let c = @sizeof(Point);
  };
}
"#,
        );

        assert!(!single.failed);
        assert!(!repeated.failed);
        assert_eq!(single.shapes, 2);
        assert_eq!(single.shapes, repeated.shapes);
    }

    #[test]
    fn keeps_shapes_of_distinct_generic_instances() {
        let stats = interpret(
            r#"struct Slot[T] {
  value: T,
}

fn main() {
  comptime {
    let a = @sizeof(Slot[i32]);
    let b = @sizeof(Slot[i64]);

    if (a != 4) { @panic("Slot[i32] size"); }
    if (b != 8) { @panic("Slot[i64] size"); }
  };
}
"#,
        );

        assert!(!stats.failed);
    }
}
