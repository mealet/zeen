use crate::{
    Parser,
    declarations::{DeclParser, IsExtern, IsPub},
    error::ParserError,
    expressions::{self, ExprParser},
    type_parser::TypeParser,
};

use zeen_ast::{
    Expression, TypeExpr,
    statements::{Statement, StatementKind},
};
use zeen_lexer::{TokenKind, token::CompilerKeyword};

pub struct StmtParser<'tok, 'ctx, 'pr> {
    p: &'pr mut Parser<'tok, 'ctx>,
    expect_optional_semicolon: bool,
    absolutely_no_semicolon: bool,
}

/// ==@ Statements Parser @==
impl<'tok, 'ctx, 'pr> StmtParser<'tok, 'ctx, 'pr> {
    pub fn new(parser: &'pr mut Parser<'tok, 'ctx>) -> Self {
        Self {
            p: parser,
            expect_optional_semicolon: false,
            absolutely_no_semicolon: false,
        }
    }

    pub fn with_optional_semicolon(mut self, flag: bool) -> Self {
        self.expect_optional_semicolon = flag;
        self
    }

    pub fn no_semicolon(mut self) -> Self {
        self.absolutely_no_semicolon = true;
        self
    }

    pub fn errors(&self) -> &[ParserError] {
        &self.p.errors
    }

    pub fn parse(&mut self) -> Option<&'ctx Statement<'ctx>> {
        if self.p.panic_mode {
            self.p.sync();
        }

        match self.p.current().kind {
            TokenKind::Keyword(CompilerKeyword::Let | CompilerKeyword::Const) => self.parse_let(),
            TokenKind::Keyword(CompilerKeyword::Return) => self.parse_return(),
            TokenKind::Keyword(CompilerKeyword::Break) => self.parse_break(),
            TokenKind::Keyword(CompilerKeyword::Continue) => self.parse_continue(),
            TokenKind::Keyword(CompilerKeyword::While) => self.parse_while(),
            TokenKind::Keyword(CompilerKeyword::For) => self.parse_for(),
            TokenKind::Keyword(CompilerKeyword::If) => self.parse_if(),
            TokenKind::OpenBrace => self.parse_block(),
            TokenKind::Keyword(CompilerKeyword::Fn | CompilerKeyword::Public) => {
                self.parse_fn_decl()
            }

            _ => self.parse_expr_or_assign(),
        }
    }

    /// Parses a nested function declaration (`fn foo() { .. }`), optionally
    /// prefixed with `pub` (parsed here, rejected later by the typechecker).
    fn parse_fn_decl(&mut self) -> Option<&'ctx Statement<'ctx>> {
        let start_span = self.p.current().span;
        let is_pub = IsPub(self.p.eat(TokenKind::Keyword(CompilerKeyword::Public)));

        let mut decl_parser = DeclParser::new(self.p);
        let decl = decl_parser.parse_fn(start_span, is_pub, IsExtern(false))?;

        Some(self.p.arena.alloc(Statement {
            kind: StatementKind::FnDecl(decl),
            span: decl.source.span,
        }))
    }

    fn expect_semicolon(&mut self) -> Option<()> {
        if self.absolutely_no_semicolon {
            return Some(());
        }

        let _ = self.p.expect(TokenKind::Semicolon, ";")?;
        Some(())
    }

    fn expect_optional_semicolon(&mut self) -> Option<()> {
        if self.absolutely_no_semicolon {
            return Some(());
        }

        if self.expect_optional_semicolon {
            let _ = self.p.expect(TokenKind::Semicolon, ";")?;
            return Some(());
        }
        Some(())
    }
}

impl<'tok, 'ctx, 'pr> StmtParser<'tok, 'ctx, 'pr> {
    pub fn parse_let(&mut self) -> Option<&'ctx Statement<'ctx>> {
        let start = self.p.current().span;
        let is_const = self.p.eat(TokenKind::Keyword(CompilerKeyword::Const));

        if !is_const {
            self.p
                .expect(TokenKind::Keyword(CompilerKeyword::Let), "let")?;
        }

        let name_token = if !self.p.at(TokenKind::Underscore) {
            self.p.expect(TokenKind::Ident, "identifier")?
        } else {
            self.p.advance()?
        };
        let name_span = name_token.span;
        let name_slice =
            self.p.src[name_span.offset()..name_span.offset() + name_span.len()].to_owned();

        let name = self.p.get_or_intern(name_slice);

        let mut explicit_type: Option<&'ctx TypeExpr<'ctx>> = None;
        let mut value: Option<&'ctx Expression<'ctx>> = None;

        if self.p.eat(TokenKind::Colon) {
            let mut type_parser = TypeParser::new(self.p);
            let type_expr = type_parser.parse()?;

            explicit_type = Some(type_expr);
        }

        if self.p.eat(TokenKind::Eq) {
            let mut expr_parser = ExprParser::new(self.p);
            let expr = expr_parser.parse()?;

            value = Some(expr);
        }

        self.expect_semicolon()?;

        let span = if let Some(value_expr) = value {
            value_expr.merge_span(start)
        } else if let Some(type_expr) = explicit_type {
            type_expr.merge_span(start)
        } else {
            name_token.merge_span(start)
        };

        let stmt = self.p.arena.alloc(Statement {
            kind: StatementKind::Let {
                name,
                explicit_type,
                value,
                is_const,
            },
            span,
        });

        Some(stmt)
    }

    pub fn parse_return(&mut self) -> Option<&'ctx Statement<'ctx>> {
        let return_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Return), "return")?;

        let mut value: Option<&'ctx Expression<'ctx>> = None;

        if !self.p.at(TokenKind::Semicolon) {
            let mut expr_parser = ExprParser::new(self.p);
            value = Some(expr_parser.parse()?);
        }

        self.expect_semicolon()?;

        let span = if let Some(value_expr) = value {
            value_expr.merge_span(return_kw.span)
        } else {
            return_kw.span
        };

        let stmt = self.p.arena.alloc(Statement {
            kind: StatementKind::Return { value },
            span,
        });

        Some(stmt)
    }

    pub fn parse_break(&mut self) -> Option<&'ctx Statement<'ctx>> {
        let break_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Break), "break")?;

        self.expect_optional_semicolon()?;

        let stmt = self.p.arena.alloc(Statement {
            kind: StatementKind::Break,
            span: break_kw.span,
        });

        Some(stmt)
    }

    pub fn parse_continue(&mut self) -> Option<&'ctx Statement<'ctx>> {
        let break_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Continue), "continue")?;

        self.expect_optional_semicolon()?;

        let stmt = self.p.arena.alloc(Statement {
            kind: StatementKind::Continue,
            span: break_kw.span,
        });

        Some(stmt)
    }

    pub fn parse_while(&mut self) -> Option<&'ctx Statement<'ctx>> {
        let while_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::While), "while")?;

        let mut expr_parser = ExprParser::new(self.p);

        let condition = expr_parser.parse_grouped()?;

        let mut stmt_parser = StmtParser::new(self.p).with_optional_semicolon(false);
        let block = stmt_parser.parse()?;

        let _ = self.p.eat(TokenKind::Semicolon);

        let stmt = self.p.arena.alloc(Statement {
            kind: StatementKind::While { condition, block },
            span: while_kw.merge_span(block.span),
        });

        Some(stmt)
    }

    pub fn parse_for(&mut self) -> Option<&'ctx Statement<'ctx>> {
        let for_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::For), "for")?;

        let _ = self.p.expect(TokenKind::OpenParen, "(")?;

        let var_token = self.p.expect(TokenKind::Ident, "identifier")?;
        let var_slice = self.p.src
            [var_token.span.offset()..var_token.span.offset() + var_token.span.len()]
            .to_owned();

        let varname_id = self.p.get_or_intern(var_slice);
        let varname_span = var_token.span;

        let _ = self.p.expect(TokenKind::Colon, ":")?;

        let mut expr_parser = ExprParser::new(self.p);
        let iterator = expr_parser.parse()?;

        let _ = self.p.expect(TokenKind::CloseParen, ")")?;

        let mut stmt_parser = StmtParser::new(self.p).with_optional_semicolon(false);
        let block = stmt_parser.parse()?;

        let _ = self.p.eat(TokenKind::Semicolon);

        let stmt = self.p.arena.alloc(Statement {
            kind: StatementKind::For {
                varname: (varname_id, varname_span),
                iterator,
                block,
            },
            span: for_kw.merge_span(block.span),
        });

        Some(stmt)
    }

    pub fn parse_block(&mut self) -> Option<&'ctx Statement<'ctx>> {
        let mut expr_parser = ExprParser::new(self.p);
        let expr = expr_parser.parse()?;

        self.finish_expr_stmt(expr)
    }

    pub fn parse_if(&mut self) -> Option<&'ctx Statement<'ctx>> {
        let mut expr_parser = ExprParser::new(self.p).if_in_statement();
        let expr = expr_parser.parse()?;

        self.finish_expr_stmt(expr)
    }

    /// Completes an expression statement (`if`, bare block, …). Directly
    /// inside a `{ ... }` block the trailing semicolon is optional, and when
    /// the block is about to close the statement becomes its trailing value.
    /// In an if/while/for body position a semicolon belongs to the enclosing
    /// statement, so it is left untouched.
    fn finish_expr_stmt(&mut self, expr: &'ctx Expression<'ctx>) -> Option<&'ctx Statement<'ctx>> {
        if !self.expect_optional_semicolon {
            return Some(self.p.arena.alloc(Statement {
                kind: StatementKind::Expr(expr),
                span: expr.span,
            }));
        }

        let _ = self.p.eat(TokenKind::Semicolon);

        let kind = if self.p.at(TokenKind::CloseBrace) {
            StatementKind::TrailingExpr(expr)
        } else {
            StatementKind::Expr(expr)
        };

        Some(self.p.arena.alloc(Statement {
            kind,
            span: expr.span,
        }))
    }

    pub fn parse_expr_or_assign(&mut self) -> Option<&'ctx Statement<'ctx>> {
        let start = self.p.current().span;

        let lhs;

        if self.p.at(TokenKind::Eof) {
            // apparently we've already reported that and reached EOF due sync
            return None;
        }

        {
            let mut expr_parser = ExprParser::new(self.p);
            lhs = expr_parser.parse_non_binary()?;
        }

        // assignment (a = b)

        if self.p.eat(TokenKind::Eq) {
            let mut expr_parser = ExprParser::new(self.p);
            let rhs = expr_parser.parse()?;

            self.expect_semicolon()?;

            let stmt = self.p.arena.alloc(Statement {
                kind: StatementKind::Assign {
                    object: lhs,
                    value: rhs,
                },
                span: rhs.merge_span(start),
            });

            return Some(stmt);
        }

        // compound assignment (a += b), but only when the binary operator is
        // directly followed by `=`. Otherwise the operator starts a plain
        // binary expression statement, e.g. `a + b` or a trailing `a + b`.

        if self.p.peek().is_some_and(|next| next.kind == TokenKind::Eq)
            && let Some(bin_info) = expressions::BinaryInfo::new(self.p.current())
        {
            const NOT_ALLOWED: &[zeen_ast::expressions::BinaryOp] = &[
                zeen_ast::expressions::BinaryOp::Eq,
                zeen_ast::expressions::BinaryOp::Ne,
                zeen_ast::expressions::BinaryOp::Lt,
                zeen_ast::expressions::BinaryOp::Gt,
                zeen_ast::expressions::BinaryOp::Le,
                zeen_ast::expressions::BinaryOp::Ge,
            ];

            if NOT_ALLOWED.contains(&bin_info.tag) {
                self.p.report(ParserError::UnsupportedAction {
                    label: "provided operator is not supported for compound assign".into(),
                    src: self.p.named_src(),
                    span: self.p.current().span,
                });

                return None;
            }

            let _ = self.p.advance_not_eof();
            let _ = self.p.expect(TokenKind::Eq, "=")?;

            let mut expr_parser = ExprParser::new(self.p);
            let rhs = expr_parser.parse()?;

            self.expect_semicolon();

            let stmt = self.p.arena.alloc(Statement {
                kind: StatementKind::CompoundAssign {
                    object: lhs,
                    value: rhs,
                    op: bin_info.tag,
                },
                span: rhs.merge_span(start),
            });

            return Some(stmt);
        }

        // expr in statement (may be a full binary expression)

        let expr = {
            let mut expr_parser = ExprParser::new(self.p);
            expr_parser.parse_binary_rest(lhs, expressions::Precedence::Lowest)?
        };

        let mut kind = StatementKind::Expr(expr);

        if self.p.at(TokenKind::CloseBrace) {
            kind = StatementKind::TrailingExpr(expr);
        } else {
            self.expect_optional_semicolon()?;
        }

        let stmt = self.p.arena.alloc(Statement {
            kind,
            span: expr.span,
        });

        Some(stmt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    use std::sync::Arc;
    use std::{cell::RefCell, rc::Rc};

    use zeen_ast::{Expression, ExpressionKind};

    macro_rules! make_stmt_parser {
        ($src:expr, $tokens:ident, $bump:ident, $rodeo:ident, $parser:ident, $ep: ident) => {
            let src_arc = Arc::new($src.to_string());
            let $rodeo = Rc::new(RefCell::new(lasso::Rodeo::default()));
            let $bump = bumpalo::Bump::new();
            let mut $tokens = zeen_lexer::tokenize($src);
            let mut $parser = Parser::new(
                Rc::new("tests.zn".to_string()),
                src_arc,
                &mut $tokens,
                &$bump,
                Rc::clone(&$rodeo),
            );
            let mut $ep = StmtParser::new(&mut $parser);
        };
    }

    #[test]
    fn assign_basic() {
        const SRC: &str = "a = b;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Assign {
                    object: Expression {
                        kind: ExpressionKind::Ident {
                            name: _,
                            generic_args: None
                        },
                        ..
                    },
                    value: Expression {
                        kind: ExpressionKind::Ident {
                            name: _,
                            generic_args: None,
                        },
                        ..
                    }
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn assign_field() {
        const SRC: &str = "a.field = b;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Assign {
                    object: Expression {
                        kind: ExpressionKind::FieldAccess {
                            object: Expression {
                                kind: ExpressionKind::Ident { .. },
                                ..
                            },
                            field: Expression {
                                kind: ExpressionKind::Ident { .. },
                                ..
                            }
                        },
                        ..
                    },
                    value: Expression {
                        kind: ExpressionKind::Ident {
                            name: _,
                            generic_args: None,
                        },
                        ..
                    }
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn assign_slice() {
        const SRC: &str = "a[123] = b;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Assign {
                    object: Expression {
                        kind: ExpressionKind::SliceAccess {
                            object: Expression {
                                kind: ExpressionKind::Ident { .. },
                                ..
                            },
                            index: Expression {
                                kind: ExpressionKind::Literal(zeen_ast::expressions::Literal::Int(
                                    123
                                )),
                                ..
                            }
                        },
                        ..
                    },
                    value: Expression {
                        kind: ExpressionKind::Ident {
                            name: _,
                            generic_args: None,
                        },
                        ..
                    }
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn assign_wtf() {
        const SRC: &str = "123 = a;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Assign {
                    object: Expression {
                        kind: ExpressionKind::Literal(zeen_ast::expressions::Literal::Int(123)),
                        ..
                    },
                    value: Expression {
                        kind: ExpressionKind::Ident {
                            name: _,
                            generic_args: None,
                        },
                        ..
                    }
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn let_only_name() {
        const SRC: &str = "let a;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Let {
                    name: _,
                    explicit_type: None,
                    value: None,
                    is_const: false
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn let_only_value() {
        const SRC: &str = "let a = 123;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Let {
                    name: _,
                    explicit_type: None,
                    value: Some(Expression {
                        kind: ExpressionKind::Literal(zeen_ast::expressions::Literal::Int(123)),
                        ..
                    }),
                    is_const: false
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn let_only_type() {
        const SRC: &str = "let a: i32;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Let {
                    name: _,
                    explicit_type: Some(TypeExpr {
                        kind: zeen_ast::TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    }),
                    value: None,
                    is_const: false
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn let_value_and_type() {
        const SRC: &str = "let a: i32 = 123;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Let {
                    name: _,
                    explicit_type: Some(TypeExpr {
                        kind: zeen_ast::TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    }),
                    value: Some(Expression {
                        kind: ExpressionKind::Literal(zeen_ast::expressions::Literal::Int(123)),
                        ..
                    }),
                    is_const: false
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn let_const_value_and_type() {
        const SRC: &str = "const a: i32 = 123;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Let {
                    name: _,
                    explicit_type: Some(TypeExpr {
                        kind: zeen_ast::TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    }),
                    value: Some(Expression {
                        kind: ExpressionKind::Literal(zeen_ast::expressions::Literal::Int(123)),
                        ..
                    }),
                    is_const: true
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn return_no_value() {
        const SRC: &str = "return;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Return { value: None },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn return_with_value() {
        const SRC: &str = "return 123;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Return {
                    value: Some(Expression {
                        kind: ExpressionKind::Literal(zeen_ast::expressions::Literal::Int(123)),
                        ..
                    })
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn break_stmt() {
        const SRC: &str = "break;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Break,
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn continue_stmt() {
        const SRC: &str = "continue;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Continue,
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn while_single() {
        const SRC: &str = "while (1 == 1) let a = 123;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::While {
                    condition: _,
                    block: Statement {
                        kind: StatementKind::Let { .. },
                        ..
                    }
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn while_block() {
        const SRC: &str = "while (1 == 1) { let a = 123; };";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::While {
                    condition: _,
                    block: Statement {
                        kind: StatementKind::Expr(..),
                        ..
                    }
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn for_single() {
        const SRC: &str = "for (i : 123) let a = 123;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::For {
                    varname: _,
                    iterator: Expression {
                        kind: ExpressionKind::Literal(zeen_ast::expressions::Literal::Int(123)),
                        ..
                    },
                    block: Statement {
                        kind: StatementKind::Let { .. },
                        ..
                    }
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn for_block() {
        const SRC: &str = "for (i : 123) { let a = 123; };";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::For {
                    varname: _,
                    iterator: Expression {
                        kind: ExpressionKind::Literal(zeen_ast::expressions::Literal::Int(123)),
                        ..
                    },
                    block: Statement {
                        kind: StatementKind::Expr(..),
                        ..
                    }
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn binary_expr_is_not_compound_assign() {
        const SRC: &str = "a + b;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Expr(Expression {
                    kind: ExpressionKind::Binary {
                        op: zeen_ast::expressions::BinaryOp::Add,
                        ..
                    },
                    ..
                }),
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn binary_expr_with_comparison_is_plain_expr() {
        const SRC: &str = "a == b;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Expr(Expression {
                    kind: ExpressionKind::Binary {
                        op: zeen_ast::expressions::BinaryOp::Eq,
                        ..
                    },
                    ..
                }),
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn trailing_binary_expr_in_block() {
        const SRC: &str = "{ slice[i] + slice[0] }";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Expr(Expression {
                    kind: ExpressionKind::Block {
                        trailing: Some(..),
                        ..
                    },
                    ..
                }),
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn compound_assign_still_parses() {
        const SRC: &str = "a += b;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::CompoundAssign {
                    op: zeen_ast::expressions::BinaryOp::Add,
                    ..
                },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn if_at_block_end_is_trailing() {
        const SRC: &str = "{ if (a) { 1 } else { 2 } }";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, _stmt_parser);
        let mut stmt_parser = StmtParser::new(&mut parser).with_optional_semicolon(true);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Expr(Expression {
                    kind: ExpressionKind::Block {
                        trailing: Some(Expression {
                            kind: ExpressionKind::If { .. },
                            ..
                        }),
                        ..
                    },
                    ..
                }),
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn block_expr_at_block_end_is_trailing() {
        const SRC: &str = "{ { 42 } }";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, _stmt_parser);
        let mut stmt_parser = StmtParser::new(&mut parser).with_optional_semicolon(true);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Expr(Expression {
                    kind: ExpressionKind::Block {
                        trailing: Some(Expression {
                            kind: ExpressionKind::Block { .. },
                            ..
                        }),
                        ..
                    },
                    ..
                }),
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn if_semicolon_is_optional_inside_block() {
        const SRC: &str = "if (a) { 1 }; let b = 2;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, _stmt_parser);
        let mut stmt_parser = StmtParser::new(&mut parser).with_optional_semicolon(true);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Expr(Expression {
                    kind: ExpressionKind::If { .. },
                    ..
                }),
                ..
            }
        );

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Let { .. },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn block_expr_semicolon_is_optional_inside_block() {
        const SRC: &str = "{ 42 } let b = 2;";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, _stmt_parser);
        let mut stmt_parser = StmtParser::new(&mut parser).with_optional_semicolon(true);

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Expr(Expression {
                    kind: ExpressionKind::Block { .. },
                    ..
                }),
                ..
            }
        );

        assert_matches!(
            stmt_parser.parse().unwrap(),
            Statement {
                kind: StatementKind::Let { .. },
                ..
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn while_and_for_at_block_end_parse_without_semicolon() {
        for src in ["while (a) { 1 }", "for (i : 10) { 1 }"] {
            let src_arc = Arc::new(src.to_string());
            let rodeo = Rc::new(RefCell::new(lasso::Rodeo::default()));
            let bump = bumpalo::Bump::new();
            let mut tokens = zeen_lexer::tokenize(src);
            let mut parser = Parser::new(
                Rc::new("tests.zn".to_string()),
                src_arc,
                &mut tokens,
                &bump,
                Rc::clone(&rodeo),
            );
            let mut stmt_parser = StmtParser::new(&mut parser).with_optional_semicolon(true);

            let stmt = stmt_parser.parse().expect("loop statement must parse");
            assert!(
                matches!(
                    stmt.kind,
                    StatementKind::While { .. } | StatementKind::For { .. }
                ),
                "expected loop statement at block end, got: {stmt:?}"
            );
            assert!(stmt_parser.parse().is_none());
        }
    }

    #[test]
    fn nested_fn_declaration_parses_as_statement() {
        const SRC: &str = "fn foo() void { @println(\"hi\"); }";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        let stmt = stmt_parser.parse().unwrap();

        assert_matches!(
            stmt.kind,
            StatementKind::FnDecl(decl) => {
                assert_matches!(
                    decl.kind,
                    zeen_ast::DeclarationKind::FnDecl {
                        generics: None,
                        return_type: Some(_),
                        body: Some(_),
                        is_pub: false,
                        is_extern: false,
                        ..
                    }
                );
            }
        );

        assert!(stmt_parser.parse().is_none());
    }

    #[test]
    fn nested_pub_fn_declaration_parses_as_statement() {
        const SRC: &str = "pub fn foo() void {}";

        make_stmt_parser!(SRC, tokens, bump, rodeo, parser, stmt_parser);

        let stmt = stmt_parser.parse().unwrap();

        assert_matches!(
            stmt.kind,
            StatementKind::FnDecl(decl) => {
                assert_matches!(
                    decl.kind,
                    zeen_ast::DeclarationKind::FnDecl { is_pub: true, .. }
                );
            }
        );

        assert!(stmt_parser.parse().is_none());
    }
}
