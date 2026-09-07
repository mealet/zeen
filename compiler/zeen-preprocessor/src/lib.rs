use std::{cell::RefCell, rc::Rc};

use bumpalo::Bump;
use lasso::{Rodeo, Spur};
use miette::SourceSpan;
use smallvec::SmallVec;

use zeen_ast::declarations::{AliasDecl, FnParam, GenericType, StructField};
use zeen_ast::expressions::Literal;
use zeen_ast::{
    Declaration, DeclarationKind, DirectiveValue, ExprConditionalBlock, Expression, ExpressionKind,
    PreprocessorDirective, Statement, StatementKind, StmtConditionalBlock, TypeExpr, TypeKind,
};
use zeen_driver::{CompilationMode, Target};

/// Resolves the preprocessor: removes non-matching `@name[...]` declaration
/// blocks and replaces `@var[...]` expressions with concrete literals. Runs
/// right after parsing, before resolve/HIR, so platform-specific code that
/// does not exist on the current target is dropped from the AST.
pub struct Preprocessor<'a, 'b> {
    arena: &'a Bump,
    interner: &'b Rc<RefCell<Rodeo>>,
    target: &'b Target,
    mode: CompilationMode,
}

impl<'a, 'b> Preprocessor<'a, 'b> {
    fn alloc_slice<T: Copy>(&mut self, items: &[T]) -> &'a [T] {
        self.arena.alloc_slice_copy(items)
    }
    pub fn new(
        arena: &'a Bump,
        interner: &'b Rc<RefCell<Rodeo>>,
        target: &'b Target,
        mode: CompilationMode,
    ) -> Self {
        Self {
            arena,
            interner,
            target,
            mode,
        }
    }

    /// Filters a top-level declaration list, returning a new arena slice where
    /// conditional blocks have been expanded and `@var` replaced.
    pub fn resolve_program(&mut self, decls: &[&'a Declaration<'a>]) -> &'a [&'a Declaration<'a>] {
        let out: SmallVec<[&'a Declaration<'a>; 8]> = self.resolve_block(decls);
        self.alloc_slice(&out)
    }

    fn resolve_block(
        &mut self,
        decls: &[&'a Declaration<'a>],
    ) -> SmallVec<[&'a Declaration<'a>; 8]> {
        let mut out: SmallVec<[&'a Declaration<'a>; 8]> = SmallVec::new();
        for decl in decls {
            if let DeclarationKind::ConditionalBlock(block) = decl.kind {
                self.expand_conditional(block, &mut out);
            } else {
                out.push(self.resolve_decl(decl));
            }
        }
        out
    }

    fn expand_conditional(
        &mut self,
        block: &'a zeen_ast::ConditionalBlock<'a>,
        out: &mut SmallVec<[&'a Declaration<'a>; 8]>,
    ) {
        let mut current = Some(block);
        while let Some(b) = current {
            if b.bare_else || self.matches(b.directive, b.values) {
                let body = self.resolve_block(b.body);
                out.extend(body);
                return;
            }
            current = match b.else_block {
                Some(else_decl) => match else_decl.kind {
                    DeclarationKind::ConditionalBlock(else_block) => Some(else_block),
                    _ => None,
                },
                None => None,
            };
        }
    }

    fn matches(&self, directive: PreprocessorDirective, values: &[DirectiveValue<'a>]) -> bool {
        match directive {
            PreprocessorDirective::Debug => self.mode == CompilationMode::Debug,
            PreprocessorDirective::Release => self.mode == CompilationMode::Release,
            other => {
                let actual = self.actual_value(other);
                values.iter().any(|v| v.value == actual)
            }
        }
    }

    fn actual_value(&self, directive: PreprocessorDirective) -> &str {
        match directive {
            PreprocessorDirective::Os => &self.target.os,
            PreprocessorDirective::Arch => &self.target.arch,
            PreprocessorDirective::Env => &self.target.env,
            PreprocessorDirective::Target => &self.target.triple,
            PreprocessorDirective::Family => &self.target.family,
            PreprocessorDirective::Debug | PreprocessorDirective::Release => unreachable!(),
        }
    }

    fn resolve_decl(&mut self, decl: &'a Declaration<'a>) -> &'a Declaration<'a> {
        let kind = self.resolve_decl_kind(decl);
        if kind == decl.kind {
            return decl;
        }
        self.arena.alloc(Declaration {
            kind,
            source: decl.source.clone(),
        })
    }

    fn resolve_decl_kind(&mut self, decl: &'a Declaration<'a>) -> DeclarationKind<'a> {
        match decl.kind {
            DeclarationKind::FnDecl {
                name,
                generics,
                params,
                return_type,
                body,
                is_pub,
                is_extern,
            } => DeclarationKind::FnDecl {
                name,
                generics: self.resolve_generics(generics),
                params: self.resolve_params(params),
                return_type: return_type.map(|ty| self.resolve_type(ty)),
                body: body.map(|s| self.resolve_stmt(s)),
                is_pub,
                is_extern,
            },

            DeclarationKind::StructDecl {
                name,
                is_pub,
                generics,
                fields,
                methods,
            } => DeclarationKind::StructDecl {
                name,
                is_pub,
                generics: self.resolve_generics(generics),
                fields: self.resolve_fields(fields),
                methods: self.resolve_nested_decls(methods),
            },

            DeclarationKind::InterfaceDecl {
                name,
                is_pub,
                generics,
                methods,
            } => DeclarationKind::InterfaceDecl {
                name,
                is_pub,
                generics: self.resolve_generics(generics),
                methods: self.resolve_nested_decls(methods),
            },

            DeclarationKind::ImplementDecl {
                interface,
                object,
                generics,
                methods,
            } => {
                let object = self.resolve_object_generics(object);
                DeclarationKind::ImplementDecl {
                    interface,
                    object,
                    generics: self.resolve_generics(generics),
                    methods: self.resolve_nested_decls(methods),
                }
            }

            DeclarationKind::EnumDecl {
                name,
                variants,
                is_pub,
            } => DeclarationKind::EnumDecl {
                name,
                variants,
                is_pub,
            },

            DeclarationKind::ExternVar { name, ty, is_pub } => DeclarationKind::ExternVar {
                name,
                ty: self.resolve_type(ty),
                is_pub,
            },

            DeclarationKind::GlobalVar {
                name,
                ty,
                value,
                is_const,
                is_pub,
            } => DeclarationKind::GlobalVar {
                name,
                ty: self.resolve_type(ty),
                value: self.resolve_expr(value),
                is_const,
                is_pub,
            },

            DeclarationKind::ExternLink { path } => DeclarationKind::ExternLink { path },
            DeclarationKind::ExternInclude { path } => DeclarationKind::ExternInclude { path },
            DeclarationKind::Use { module } => DeclarationKind::Use { module },

            DeclarationKind::Alias(alias) => DeclarationKind::Alias(AliasDecl {
                name: alias.name,
                is_pub: alias.is_pub,
                generics: self.resolve_generics(alias.generics),
                ty: self.resolve_type(alias.ty),
            }),

            DeclarationKind::ConditionalBlock(_) => {
                unreachable!("conditional blocks are expanded by resolve_block")
            }
        }
    }

    fn resolve_nested_decls(&mut self, decls: &[&'a Declaration<'a>]) -> &'a [&'a Declaration<'a>] {
        let out = self.resolve_block(decls);
        self.alloc_slice(&out)
    }

    fn resolve_generics(
        &mut self,
        generics: Option<&'a [GenericType<'a>]>,
    ) -> Option<&'a [GenericType<'a>]> {
        generics.map(|generics| self.alloc_slice(generics))
    }

    fn resolve_params(&mut self, params: &'a [FnParam<'a>]) -> &'a [FnParam<'a>] {
        let fixed: SmallVec<[FnParam<'a>; 4]> = params
            .iter()
            .map(|p| FnParam {
                name: p.name,
                ty: self.resolve_type(p.ty),
                span: p.span,
            })
            .collect();
        self.alloc_slice(&fixed)
    }

    fn resolve_fields(&mut self, fields: &'a [StructField<'a>]) -> &'a [StructField<'a>] {
        let fixed: SmallVec<[StructField<'a>; 4]> = fields
            .iter()
            .map(|f| StructField {
                name: f.name,
                ty: self.resolve_type(f.ty),
                is_pub: f.is_pub,
            })
            .collect();
        self.alloc_slice(&fixed)
    }

    fn resolve_object_generics(
        &mut self,
        object: (Spur, SourceSpan, &'a [&'a TypeExpr<'a>]),
    ) -> (Spur, SourceSpan, &'a [&'a TypeExpr<'a>]) {
        let (name, span, slots) = object;
        let slots: SmallVec<[&'a TypeExpr<'a>; 4]> =
            slots.iter().map(|ty| self.resolve_type(ty)).collect();
        (name, span, self.alloc_slice(&slots))
    }

    fn resolve_type(&mut self, ty: &'a TypeExpr<'a>) -> &'a TypeExpr<'a> {
        let kind = self.resolve_type_kind(ty);
        if kind == ty.kind {
            return ty;
        }
        self.arena.alloc(TypeExpr {
            kind,
            span: ty.span,
        })
    }

    fn resolve_type_kind(&mut self, ty: &'a TypeExpr<'a>) -> TypeKind<'a> {
        match ty.kind {
            TypeKind::Builtin(b) => TypeKind::Builtin(b),
            TypeKind::SelfType => TypeKind::SelfType,
            TypeKind::SelfAlias => TypeKind::SelfAlias,
            TypeKind::VaArgs => TypeKind::VaArgs,

            TypeKind::Named { name, generic_args } => {
                let generic_args = generic_args.map(|args| {
                    let fixed: SmallVec<[&'a TypeExpr<'a>; 4]> =
                        args.iter().map(|ty| self.resolve_type(ty)).collect();
                    self.alloc_slice(&fixed)
                });
                TypeKind::Named { name, generic_args }
            }

            TypeKind::Const(inner) => TypeKind::Const(self.resolve_type(inner)),
            TypeKind::TypeOf(expr) => TypeKind::TypeOf(self.resolve_expr(expr)),
            TypeKind::SinglePointer(inner) => TypeKind::SinglePointer(self.resolve_type(inner)),
            TypeKind::ManyPointer(inner) => TypeKind::ManyPointer(self.resolve_type(inner)),

            TypeKind::Array { element, len } => TypeKind::Array {
                element: self.resolve_type(element),
                len: len.map(|e| self.resolve_expr(e)),
            },

            TypeKind::Fn {
                params,
                generic_args,
                ret,
            } => TypeKind::Fn {
                params: self.resolve_type_slice(params),
                generic_args: self.resolve_generics(generic_args),
                ret: self.resolve_type(ret),
            },

            TypeKind::FatFn { params, ret, once } => TypeKind::FatFn {
                params: self.resolve_type_slice(params),
                ret: self.resolve_type(ret),
                once,
            },
        }
    }

    fn resolve_type_slice(&mut self, params: &'a [&'a TypeExpr<'a>]) -> &'a [&'a TypeExpr<'a>] {
        let fixed: SmallVec<[&'a TypeExpr<'a>; 4]> =
            params.iter().map(|ty| self.resolve_type(ty)).collect();
        self.alloc_slice(&fixed)
    }

    fn resolve_stmt(&mut self, stmt: &'a Statement<'a>) -> &'a Statement<'a> {
        let kind = self.resolve_stmt_kind(stmt);
        if kind == stmt.kind {
            return stmt;
        }
        self.arena.alloc(Statement {
            kind,
            span: stmt.span,
        })
    }

    /// Resolves a statement list, flattening statement-level conditional blocks
    /// into the statements of the single matching branch.
    fn resolve_stmt_list(&mut self, stmts: &'a [&'a Statement<'a>]) -> &'a [&'a Statement<'a>] {
        let mut out: SmallVec<[&'a Statement<'a>; 8]> = SmallVec::new();
        for s in stmts {
            if let StatementKind::ConditionalBlock(block) = s.kind {
                self.expand_stmt_conditional(block, &mut out);
            } else {
                out.push(self.resolve_stmt(s));
            }
        }
        self.alloc_slice(&out)
    }

    fn expand_stmt_conditional(
        &mut self,
        block: &'a StmtConditionalBlock<'a>,
        out: &mut SmallVec<[&'a Statement<'a>; 8]>,
    ) {
        if let Some(branch) = self.matched_stmt_branch(block) {
            let stmts = self.resolve_stmt_list(branch);
            out.extend_from_slice(stmts);
        }
    }

    /// Returns the statements of the single matching branch of a statement-level
    /// conditional, walking the `else if` / `else` chain.
    fn matched_stmt_branch(
        &self,
        block: &'a StmtConditionalBlock<'a>,
    ) -> Option<&'a [&'a Statement<'a>]> {
        let mut current = Some(block);
        while let Some(b) = current {
            if b.bare_else || self.matches(b.directive, b.values) {
                return Some(b.stmts);
            }
            current = match b.else_block {
                Some(else_stmt) => match else_stmt.kind {
                    StatementKind::ConditionalBlock(else_block) => Some(else_block),
                    _ => None,
                },
                None => None,
            };
        }
        None
    }

    /// Expands a statement-level conditional used as a single statement body
    /// (e.g. the body of an `if`/`while`/`for`). The matching branch is resolved
    /// and wrapped back into a block expression statement.
    fn expand_stmt_conditional_single(
        &mut self,
        block: &'a StmtConditionalBlock<'a>,
        span: miette::SourceSpan,
    ) -> Option<&'a Statement<'a>> {
        let branch = self.matched_stmt_branch(block)?;
        let stmts = self.resolve_stmt_list(branch);
        let expr = self.arena.alloc(Expression {
            kind: ExpressionKind::Block {
                stmts,
                trailing: None,
            },
            span,
        });
        Some(self.arena.alloc(Statement {
            kind: StatementKind::Expr(expr),
            span,
        }))
    }

    fn resolve_stmt_kind(&mut self, stmt: &'a Statement<'a>) -> StatementKind<'a> {
        match stmt.kind {
            StatementKind::Let {
                name,
                explicit_type,
                value,
                is_const,
            } => StatementKind::Let {
                name,
                explicit_type: explicit_type.map(|ty| self.resolve_type(ty)),
                value: value.map(|e| self.resolve_expr(e)),
                is_const,
            },

            StatementKind::Assign { object, value } => StatementKind::Assign {
                object: self.resolve_expr(object),
                value: self.resolve_expr(value),
            },

            StatementKind::CompoundAssign { object, value, op } => StatementKind::CompoundAssign {
                object: self.resolve_expr(object),
                value: self.resolve_expr(value),
                op,
            },

            StatementKind::Return { value } => StatementKind::Return {
                value: value.map(|e| self.resolve_expr(e)),
            },

            StatementKind::Break => StatementKind::Break,
            StatementKind::Continue => StatementKind::Continue,

            StatementKind::While { condition, block } => StatementKind::While {
                condition: self.resolve_expr(condition),
                block: self.resolve_stmt(block),
            },

            StatementKind::For {
                varname,
                iterator,
                block,
            } => StatementKind::For {
                varname,
                iterator: self.resolve_expr(iterator),
                block: self.resolve_stmt(block),
            },

            StatementKind::Expr(expr) => StatementKind::Expr(self.resolve_expr(expr)),
            StatementKind::TrailingExpr(expr) => {
                StatementKind::TrailingExpr(self.resolve_expr(expr))
            }

            StatementKind::FnDecl(decl) => StatementKind::FnDecl(self.resolve_decl(decl)),

            StatementKind::ConditionalBlock(block) => {
                match self.expand_stmt_conditional_single(block, stmt.span) {
                    Some(s) => s.kind,
                    None => StatementKind::Expr(self.arena.alloc(Expression {
                        kind: ExpressionKind::Block {
                            stmts: &[],
                            trailing: None,
                        },
                        span: stmt.span,
                    })),
                }
            }
        }
    }

    fn resolve_expr(&mut self, expr: &'a Expression<'a>) -> &'a Expression<'a> {
        if let ExpressionKind::ConditionalBlock(block) = expr.kind {
            return self.expand_expr_conditional(block);
        }
        let kind = self.resolve_expr_kind(expr);
        if kind == expr.kind {
            return expr;
        }
        self.arena.alloc(Expression {
            kind,
            span: expr.span,
        })
    }

    /// Replaces an expression-level conditional block with the body of the
    /// single matching branch.
    fn expand_expr_conditional(
        &mut self,
        block: &'a ExprConditionalBlock<'a>,
    ) -> &'a Expression<'a> {
        let mut current = Some(block);
        while let Some(b) = current {
            if b.bare_else || self.matches(b.directive, b.values) {
                return self.resolve_expr(b.body);
            }
            current = match b.else_block {
                Some(else_expr) => match else_expr.kind {
                    ExpressionKind::ConditionalBlock(else_block) => Some(else_block),
                    _ => None,
                },
                None => None,
            };
        }
        unreachable!("conditional expression has no matching branch or else")
    }

    fn resolve_expr_kind(&mut self, expr: &'a Expression<'a>) -> ExpressionKind<'a> {
        match expr.kind {
            ExpressionKind::Literal(lit) => ExpressionKind::Literal(lit),

            ExpressionKind::TargetVar(kind) => {
                let literal = self.target_var_value(kind);
                ExpressionKind::Literal(literal)
            }

            ExpressionKind::Ident { name, generic_args } => ExpressionKind::Ident {
                name,
                generic_args: generic_args.map(|args| {
                    let fixed: SmallVec<[&'a TypeExpr<'a>; 4]> =
                        args.iter().map(|ty| self.resolve_type(ty)).collect();
                    self.alloc_slice(&fixed)
                }),
            },

            ExpressionKind::Binary { lhs, rhs, op } => ExpressionKind::Binary {
                lhs: self.resolve_expr(lhs),
                rhs: self.resolve_expr(rhs),
                op,
            },

            ExpressionKind::Unary { expr, op } => ExpressionKind::Unary {
                expr: self.resolve_expr(expr),
                op,
            },

            ExpressionKind::Call { callee, args } => ExpressionKind::Call {
                callee: self.resolve_expr(callee),
                args: self.resolve_expr_slice(args),
            },

            ExpressionKind::MacroCall { name, args } => ExpressionKind::MacroCall {
                name,
                args: self.resolve_expr_slice(args),
            },

            ExpressionKind::If {
                condition,
                then_block,
                else_block,
            } => ExpressionKind::If {
                condition: self.resolve_expr(condition),
                then_block: self.resolve_stmt(then_block),
                else_block: else_block.map(|s| self.resolve_stmt(s)),
            },

            ExpressionKind::Switch { object, arms } => ExpressionKind::Switch {
                object: self.resolve_expr(object),
                arms: self.alloc_slice(arms),
            },

            ExpressionKind::FieldAccess { object, field } => ExpressionKind::FieldAccess {
                object: self.resolve_expr(object),
                field: self.resolve_expr(field),
            },

            ExpressionKind::SliceAccess { object, index } => ExpressionKind::SliceAccess {
                object: self.resolve_expr(object),
                index: self.resolve_expr(index),
            },

            ExpressionKind::StructInit { ty, fields } => ExpressionKind::StructInit {
                ty: self.resolve_expr(ty),
                fields,
            },

            ExpressionKind::ArrayInit { elements } => ExpressionKind::ArrayInit {
                elements: self.resolve_expr_slice(elements),
            },

            ExpressionKind::ArrayRepeatInit { element, len } => ExpressionKind::ArrayRepeatInit {
                element: self.resolve_expr(element),
                len: self.resolve_expr(len),
            },

            ExpressionKind::Block { stmts, trailing } => ExpressionKind::Block {
                stmts: self.resolve_stmt_list(stmts),
                trailing: trailing.map(|e| self.resolve_expr(e)),
            },

            ExpressionKind::Closure {
                params,
                return_type,
                body,
            } => ExpressionKind::Closure {
                params: self.resolve_params(params),
                return_type: return_type.map(|ty| self.resolve_type(ty)),
                body: self.resolve_stmt(body),
            },

            ExpressionKind::Type(ty) => ExpressionKind::Type(self.resolve_type(ty)),

            ExpressionKind::ConditionalBlock(_) => {
                unreachable!("conditional expressions are expanded by resolve_expr")
            }
        }
    }

    fn resolve_expr_slice(&mut self, exprs: &'a [&'a Expression<'a>]) -> &'a [&'a Expression<'a>] {
        let fixed: SmallVec<[&'a Expression<'a>; 8]> =
            exprs.iter().map(|e| self.resolve_expr(e)).collect();
        self.alloc_slice(&fixed)
    }

    fn target_var_value(&mut self, kind: zeen_ast::expressions::TargetVarKind) -> Literal {
        match kind {
            zeen_ast::expressions::TargetVarKind::Os => {
                Literal::String(self.intern(&self.target.os))
            }
            zeen_ast::expressions::TargetVarKind::Arch => {
                Literal::String(self.intern(&self.target.arch))
            }
            zeen_ast::expressions::TargetVarKind::Env => {
                Literal::String(self.intern(&self.target.env))
            }
            zeen_ast::expressions::TargetVarKind::Target => {
                Literal::String(self.intern(&self.target.triple))
            }
            zeen_ast::expressions::TargetVarKind::Family => {
                Literal::String(self.intern(&self.target.family))
            }
            zeen_ast::expressions::TargetVarKind::Debug => {
                Literal::Bool(self.mode == CompilationMode::Debug)
            }
            zeen_ast::expressions::TargetVarKind::Release => {
                Literal::Bool(self.mode == CompilationMode::Release)
            }
        }
    }

    fn intern(&mut self, value: &str) -> Spur {
        self.interner.borrow_mut().get_or_intern(value)
    }
}

/// Convenience entry point.
pub fn resolve<'a>(
    program: &[&'a Declaration<'a>],
    arena: &'a Bump,
    interner: &Rc<RefCell<Rodeo>>,
    target: &Target,
    mode: CompilationMode,
) -> &'a [&'a Declaration<'a>] {
    Preprocessor::new(arena, interner, target, mode).resolve_program(program)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use zeen_ast::{ExpressionKind, Statement, StatementKind};
    use zeen_parser::Parser;

    fn run(
        src: &str,
        target: Target,
        mode: CompilationMode,
    ) -> (Vec<&'static Declaration<'static>>, Rodeo) {
        let arena: &'static Bump = Box::leak(Box::new(Bump::new()));
        let interner: &'static Rc<RefCell<Rodeo>> =
            Box::leak(Box::new(Rc::new(RefCell::new(Rodeo::default()))));
        let target: &'static Target = Box::leak(Box::new(target));

        let src_arc = Arc::new(src.to_string());
        let mut tokens = zeen_lexer::tokenize(src);
        let mut parser = Parser::new(
            Rc::new("test.zn".to_string()),
            Arc::clone(&src_arc),
            &mut tokens,
            arena,
            Rc::clone(interner),
        );
        let program = parser.parse_program().expect("parse should succeed");
        let resolved = resolve(program, arena, interner, target, mode);
        (resolved.to_vec(), interner.borrow().clone())
    }

    fn linux() -> Target {
        Target::parse("x86_64-unknown-linux-gnu")
    }

    #[test]
    fn keeps_matching_block() {
        let (decls, _) = run("@os[linux] { fn a() {} }", linux(), CompilationMode::Debug);
        assert_eq!(decls.len(), 1);
        assert!(matches!(decls[0].kind, DeclarationKind::FnDecl { .. }));
    }

    #[test]
    fn drops_non_matching_block() {
        let (decls, _) = run(
            "@os[windows] { fn a() {} }",
            linux(),
            CompilationMode::Debug,
        );
        assert!(decls.is_empty());
    }

    #[test]
    fn takes_else_when_guard_fails() {
        let (decls, _) = run(
            "@os[windows] { fn a() {} } else { fn b() {} }",
            linux(),
            CompilationMode::Debug,
        );
        assert_eq!(decls.len(), 1);
        assert!(matches!(decls[0].kind, DeclarationKind::FnDecl { .. }));
    }

    #[test]
    fn debug_and_release_select_by_mode() {
        let (decls, _) = run(
            "@debug { fn a() {} } @release { fn b() {} }",
            linux(),
            CompilationMode::Debug,
        );
        assert_eq!(decls.len(), 1);
        assert!(matches!(decls[0].kind, DeclarationKind::FnDecl { .. }));
    }

    #[test]
    fn target_var_becomes_string_literal() {
        let (decls, interner) = run(
            "fn main() { let os: string = @var[os]; }",
            linux(),
            CompilationMode::Debug,
        );
        let DeclarationKind::FnDecl { body, .. } = decls[0].kind else {
            panic!("expected fn decl");
        };
        let Statement { kind, .. } = body.unwrap();
        let StatementKind::Expr(expr) = kind else {
            panic!("expected expr stmt: {kind:?}");
        };
        let ExpressionKind::Block { stmts, .. } = expr.kind else {
            panic!("expected block expr: {expr:?}");
        };
        let StatementKind::Let { value, .. } = stmts[0].kind else {
            panic!("expected let stmt");
        };
        let expr = value.unwrap();
        let ExpressionKind::Literal(zeen_ast::expressions::Literal::String(spur)) = expr.kind
        else {
            panic!("expected string literal");
        };
        assert_eq!(interner.resolve(&spur), "linux");
    }

    #[test]
    fn target_var_bool_for_debug() {
        let (decls, _) = run(
            "fn main() { let d: bool = @var[debug]; }",
            linux(),
            CompilationMode::Debug,
        );
        let DeclarationKind::FnDecl { body, .. } = decls[0].kind else {
            panic!("expected fn decl");
        };
        let Statement { kind, .. } = body.unwrap();
        let StatementKind::Expr(expr) = kind else {
            panic!("expected expr stmt: {kind:?}");
        };
        let ExpressionKind::Block { stmts, .. } = expr.kind else {
            panic!("expected block expr: {expr:?}");
        };
        let StatementKind::Let { value, .. } = stmts[0].kind else {
            panic!("expected let stmt");
        };
        assert!(matches!(
            value.unwrap().kind,
            ExpressionKind::Literal(zeen_ast::expressions::Literal::Bool(true))
        ));
    }

    #[test]
    fn statement_conditional_selects_matching_branch() {
        let (decls, _) = run(
            "fn main() { @os[linux] { let a: i32 = 1; } else { let a: i32 = 2; } }",
            linux(),
            CompilationMode::Debug,
        );
        let DeclarationKind::FnDecl { body, .. } = decls[0].kind else {
            panic!("expected fn decl");
        };
        let Statement { kind, .. } = body.unwrap();
        let StatementKind::Expr(expr) = kind else {
            panic!("expected expr stmt: {kind:?}");
        };
        let ExpressionKind::Block { stmts, .. } = expr.kind else {
            panic!("expected block expr: {expr:?}");
        };
        assert_eq!(stmts.len(), 1);
        assert!(matches!(stmts[0].kind, StatementKind::Let { .. }));
    }

    #[test]
    fn statement_conditional_takes_else_when_guard_fails() {
        let (decls, interner) = run(
            "fn main() { @os[windows] { let a: i32 = 1; } else { let b: i32 = 2; } }",
            linux(),
            CompilationMode::Debug,
        );
        let DeclarationKind::FnDecl { body, .. } = decls[0].kind else {
            panic!("expected fn decl");
        };
        let Statement { kind, .. } = body.unwrap();
        let StatementKind::Expr(expr) = kind else {
            panic!("expected expr stmt: {kind:?}");
        };
        let ExpressionKind::Block { stmts, .. } = expr.kind else {
            panic!("expected block expr: {expr:?}");
        };
        assert_eq!(stmts.len(), 1);
        let StatementKind::Let { name, .. } = stmts[0].kind else {
            panic!("expected let stmt");
        };
        assert_eq!(interner.resolve(&name), "b");
    }

    #[test]
    fn expression_conditional_selects_matching_branch() {
        let (decls, _) = run(
            "fn main() { let x: i32 = @os[linux] { 10 } else { 20 }; }",
            linux(),
            CompilationMode::Debug,
        );
        let DeclarationKind::FnDecl { body, .. } = decls[0].kind else {
            panic!("expected fn decl");
        };
        let Statement { kind, .. } = body.unwrap();
        let StatementKind::Expr(expr) = kind else {
            panic!("expected expr stmt: {kind:?}");
        };
        let ExpressionKind::Block { stmts, .. } = expr.kind else {
            panic!("expected block expr: {expr:?}");
        };
        let StatementKind::Let { value, .. } = stmts[0].kind else {
            panic!("expected let stmt");
        };
        let ExpressionKind::Block { trailing, .. } = value.unwrap().kind else {
            panic!("expected block expr value: {:?}", value.unwrap().kind);
        };
        assert!(matches!(
            trailing.unwrap().kind,
            ExpressionKind::Literal(zeen_ast::expressions::Literal::Int(10))
        ));
    }

    #[test]
    fn conditional_as_while_body_expands_to_block() {
        let (decls, _) = run(
            "fn main() { let n: i32 = 0; while (n < 1) @os[linux] { let x: i32 = 1; } ; }",
            linux(),
            CompilationMode::Debug,
        );
        let DeclarationKind::FnDecl { body, .. } = decls[0].kind else {
            panic!("expected fn decl");
        };
        let Statement { kind, .. } = body.unwrap();
        let StatementKind::Expr(Expression {
            kind: ExpressionKind::Block { stmts, .. },
            ..
        }) = kind
        else {
            panic!("expected block expr stmt: {kind:?}");
        };
        assert_eq!(stmts.len(), 2);
        let StatementKind::While { block, .. } = stmts[1].kind else {
            panic!("expected while in body");
        };
        let StatementKind::Expr(Expression {
            kind: ExpressionKind::Block { stmts: inner, .. },
            ..
        }) = block.kind
        else {
            panic!("expected block while body: {:?}", block.kind);
        };
        assert_eq!(inner.len(), 1);
        assert!(matches!(inner[0].kind, StatementKind::Let { .. }));
    }
}
