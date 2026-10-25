use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use lasso::{Rodeo, Spur};
use miette::NamedSource;
use rustc_hash::FxHashSet;
use smol_str::SmolStr;

use zeen_ast::{
    Declaration, DeclarationKind, Expression, ExpressionKind, Statement, StatementKind,
    types::{BuiltinType, TypeExpr, TypeKind},
};

use crate::error::Error;

const COMPTIME_TYPES: [&str; 8] = [
    "comptime_int",
    "comptime_float",
    "comptime_bool",
    "comptime_char",
    "comptime_str",
    "zeen_type",
    "zeen_expr",
    "zeen_decl",
];

pub(crate) fn validate_program<'arena>(
    program: &'arena [&'arena Declaration<'arena>],
    interner: &Rc<RefCell<Rodeo>>,
) -> Vec<Error> {
    let mut checker = Checker {
        interner,
        src: None,
        structs: collect_comptime_structs(program),
        errors: Vec::new(),
    };

    for decl in program {
        checker.declaration(decl, false);
    }

    checker.errors
}

fn collect_comptime_structs<'arena>(
    program: &'arena [&'arena Declaration<'arena>],
) -> FxHashSet<Spur> {
    program
        .iter()
        .filter_map(|decl| match &decl.kind {
            DeclarationKind::ComptimeStruct { name, .. } => Some(name.0),
            _ => None,
        })
        .collect()
}

fn is_self_receiver(ty: &TypeExpr<'_>) -> bool {
    match &ty.kind {
        TypeKind::SelfType | TypeKind::SelfAlias => true,
        TypeKind::Const(inner) | TypeKind::SinglePointer(inner) | TypeKind::ManyPointer(inner) => {
            is_self_receiver(inner)
        }
        _ => false,
    }
}

struct Checker<'a> {
    interner: &'a Rc<RefCell<Rodeo>>,
    src: Option<NamedSource<Arc<String>>>,
    structs: FxHashSet<Spur>,
    errors: Vec<Error>,
}

impl<'arena> Checker<'_> {
    fn declaration(&mut self, decl: &'arena Declaration<'arena>, comptime: bool) {
        let previous = self.src.replace(decl.source.src());

        match &decl.kind {
            DeclarationKind::ComptimeFn {
                params,
                return_type,
                body,
                ..
            } => {
                for param in params.iter() {
                    self.check_type(param.ty, false);
                }

                if let Some(ret) = return_type {
                    self.check_type(ret, true);
                }

                if let Some(body) = body {
                    self.statement(body, true);
                }
            }

            DeclarationKind::ComptimeConst { value, .. } => self.expression(value, true),

            DeclarationKind::ComptimeBlock(block) => {
                for stmt in block.stmts.iter() {
                    self.statement(stmt, true);
                }
            }

            DeclarationKind::ComptimeStruct {
                fields, methods, ..
            } => {
                for field in fields.iter() {
                    self.check_type(field.ty, false);
                }

                for method in methods.iter() {
                    let (params, return_type, body) = match &method.kind {
                        DeclarationKind::FnDecl {
                            params,
                            return_type,
                            body,
                            ..
                        } => (*params, *return_type, *body),
                        _ => continue,
                    };

                    for param in params.iter() {
                        if is_self_receiver(param.ty) {
                            continue;
                        }

                        self.check_type(param.ty, false);
                    }

                    if let Some(ret) = return_type {
                        self.check_type(ret, true);
                    }

                    if let Some(body) = body {
                        self.statement(body, true);
                    }
                }
            }

            DeclarationKind::FnDecl {
                body: Some(body), ..
            } => self.statement(body, comptime),

            DeclarationKind::StructDecl { methods, .. }
            | DeclarationKind::EnumDecl { methods, .. }
            | DeclarationKind::InterfaceDecl { methods, .. }
            | DeclarationKind::ImplementDecl { methods, .. } => {
                for method in methods.iter() {
                    self.declaration(method, comptime);
                }
            }

            DeclarationKind::GlobalVar { value, .. } => self.expression(value, comptime),

            _ => {}
        }

        self.src = previous;
    }

    fn statement(&mut self, stmt: &'arena Statement<'arena>, comptime: bool) {
        match &stmt.kind {
            StatementKind::Let {
                explicit_type,
                value,
                ..
            } => {
                if comptime && let Some(ty) = explicit_type {
                    self.check_type(ty, false);
                }

                if let Some(value) = value {
                    self.expression(value, comptime);
                }
            }

            StatementKind::Assign { object, value }
            | StatementKind::CompoundAssign { object, value, .. } => {
                self.expression(object, comptime);
                self.expression(value, comptime);
            }

            StatementKind::Return { value } => {
                if let Some(value) = value {
                    self.expression(value, comptime);
                }
            }

            StatementKind::While { condition, block } => {
                self.expression(condition, comptime);
                self.statement(block, comptime);
            }

            StatementKind::For {
                iterator, block, ..
            } => {
                self.expression(iterator, comptime);
                self.statement(block, comptime);
            }

            StatementKind::Expr(expr) | StatementKind::TrailingExpr(expr) => {
                self.expression(expr, comptime);
            }

            StatementKind::FnDecl(decl) => self.declaration(decl, false),

            StatementKind::ConditionalBlock(block) => {
                for stmt in block.stmts.iter() {
                    self.statement(stmt, comptime);
                }

                if let Some(else_block) = block.else_block {
                    self.statement(else_block, comptime);
                }
            }

            StatementKind::Break | StatementKind::Continue => {}
        }
    }

    fn expression(&mut self, expr: &'arena Expression<'arena>, comptime: bool) {
        match &expr.kind {
            ExpressionKind::Literal(_)
            | ExpressionKind::Ident { .. }
            | ExpressionKind::Type(_)
            | ExpressionKind::TargetVar(_) => {}

            ExpressionKind::Binary { lhs, rhs, .. } => {
                self.expression(lhs, comptime);
                self.expression(rhs, comptime);
            }

            ExpressionKind::Unary { expr, .. } => self.expression(expr, comptime),

            ExpressionKind::Call { callee, args } => {
                self.expression(callee, comptime);
                for arg in args.iter() {
                    self.expression(arg, comptime);
                }
            }

            ExpressionKind::MacroCall { args, .. } => {
                for arg in args.iter() {
                    self.expression(arg, comptime);
                }
            }

            ExpressionKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.expression(condition, comptime);
                self.statement(then_block, comptime);

                if let Some(else_block) = else_block {
                    self.statement(else_block, comptime);
                }
            }

            ExpressionKind::Switch { object, arms } => {
                self.expression(object, comptime);

                for arm in arms.iter() {
                    if let Some(guard) = arm.guard {
                        self.expression(guard, comptime);
                    }

                    self.expression(arm.body, comptime);
                }
            }

            ExpressionKind::FieldAccess { object, field } => {
                self.expression(object, comptime);
                self.expression(field, comptime);
            }

            ExpressionKind::SliceAccess { object, index } => {
                self.expression(object, comptime);
                self.expression(index, comptime);
            }

            ExpressionKind::StructInit { ty, fields } => {
                self.expression(ty, comptime);

                if let Some(fields) = fields {
                    for field in fields.iter() {
                        self.expression(field.value, comptime);
                    }
                }
            }

            ExpressionKind::ArrayInit { elements } => {
                for element in elements.iter() {
                    self.expression(element, comptime);
                }
            }

            ExpressionKind::ArrayRepeatInit { element, len } => {
                self.expression(element, comptime);
                self.expression(len, comptime);
            }

            ExpressionKind::Block { stmts, trailing } => {
                for stmt in stmts.iter() {
                    self.statement(stmt, comptime);
                }

                if let Some(trailing) = trailing {
                    self.expression(trailing, comptime);
                }
            }

            ExpressionKind::Closure { body, .. } => self.statement(body, false),

            ExpressionKind::ConditionalBlock(block) => {
                self.expression(block.body, comptime);

                if let Some(else_block) = block.else_block {
                    self.expression(else_block, comptime);
                }
            }

            ExpressionKind::Range { start, end, .. } => {
                if let Some(start) = start {
                    self.expression(start, comptime);
                }

                if let Some(end) = end {
                    self.expression(end, comptime);
                }
            }

            ExpressionKind::Splice { expr } => self.expression(expr, comptime),

            ExpressionKind::ComptimeBlock { stmts, trailing } => {
                for stmt in stmts.iter() {
                    self.statement(stmt, true);
                }

                if let Some(trailing) = trailing {
                    self.expression(trailing, true);
                }
            }
        }
    }

    fn check_type(&mut self, ty: &'arena TypeExpr<'arena>, allow_void: bool) {
        match &ty.kind {
            TypeKind::Builtin(BuiltinType::void) if allow_void => {}

            TypeKind::SelfType | TypeKind::SelfAlias => {}

            TypeKind::Const(inner) => self.check_type(inner, false),

            TypeKind::Named { name, generic_args }
                if generic_args.is_none() && self.is_comptime_type(*name) => {}

            TypeKind::Array {
                element,
                len: Some(_),
            } => self.check_type(element, false),

            _ => self.report(ty, allow_void),
        }
    }

    fn current_src(&self) -> NamedSource<Arc<String>> {
        self.src
            .clone()
            .unwrap_or_else(|| NamedSource::new("comptime", Arc::new(String::new())))
    }

    fn is_comptime_type(&self, name: Spur) -> bool {
        if self.structs.contains(&name) {
            return true;
        }

        let resolved = self.interner.borrow().resolve(&name).to_string();

        COMPTIME_TYPES.contains(&resolved.as_str())
    }

    fn report(&mut self, ty: &'arena TypeExpr<'arena>, allow_void: bool) {
        let rendered = crate::display::type_display(self.interner, ty);

        let message = if matches!(ty.kind, TypeKind::Builtin(BuiltinType::void)) && !allow_void {
            "`void` is only allowed as a comptime function return type".to_string()
        } else {
            match self.comptime_alternative(ty) {
                Some(alternative) => format!(
                    "runtime type `{rendered}` is not allowed in comptime code, use `{alternative}` instead"
                ),
                None => {
                    format!("runtime type `{rendered}` is not allowed in comptime code")
                }
            }
        };

        self.errors.push(Error::RuntimeType {
            message: SmolStr::from(message),
            src: self.current_src(),
            span: ty.span,
        });
    }

    fn comptime_alternative(&self, ty: &'arena TypeExpr<'arena>) -> Option<&'static str> {
        match &ty.kind {
            TypeKind::Builtin(builtin) => match builtin {
                BuiltinType::i8
                | BuiltinType::i16
                | BuiltinType::i32
                | BuiltinType::i64
                | BuiltinType::isize
                | BuiltinType::u8
                | BuiltinType::u16
                | BuiltinType::u32
                | BuiltinType::u64
                | BuiltinType::usize => Some("comptime_int"),

                BuiltinType::f32 | BuiltinType::f64 => Some("comptime_float"),

                BuiltinType::bool => Some("comptime_bool"),

                BuiltinType::char => Some("comptime_char"),

                _ => None,
            },

            TypeKind::Named { name, .. } => {
                let resolved = self.interner.borrow().resolve(name).to_string();

                match resolved.as_str() {
                    "str" | "String" => Some("comptime_str"),
                    _ => None,
                }
            }

            TypeKind::Array { element, len: None } | TypeKind::ManyPointer(element)
                if matches!(element.kind, TypeKind::Builtin(BuiltinType::char)) =>
            {
                Some("comptime_str")
            }

            _ => None,
        }
    }
}
