use crate::{
    Parser,
    error::ParserError,
    expressions::{self, ExprParser},
    type_parser::TypeParser,
};

use smallvec::SmallVec;

use zeen_ast::{
    Expression, TypeExpr,
    statements::{self, Statement, StatementKind},
};
use zeen_lexer::{Token, TokenKind, token::CompilerKeyword};

pub struct StmtParser<'ctx, 'pr> {
    p: &'pr mut Parser<'ctx>,
    expect_optional_semicolon: bool,
}

/// ==@ Statements Parser @==
impl<'ctx, 'pr> StmtParser<'ctx, 'pr> {
    pub fn new(parser: &'pr mut Parser<'ctx>) -> Self {
        Self {
            p: parser,
            expect_optional_semicolon: false,
        }
    }

    pub fn with_optional_semicolon(mut self, flag: bool) -> Self {
        self.expect_optional_semicolon = flag;
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
            TokenKind::Keyword(CompilerKeyword::Defer) => self.parse_defer(),
            TokenKind::Keyword(CompilerKeyword::While) => self.parse_while(),
            TokenKind::Keyword(CompilerKeyword::For) => self.parse_for(),
            TokenKind::Keyword(CompilerKeyword::Switch) => self.parse_switch(),
            TokenKind::OpenBrace => self.parse_block(),

            _ => self.parse_expr_or_assign(),
        }
    }

    fn expect_semicolon(&mut self) -> Option<()> {
        let _ = self.p.expect(TokenKind::Semicolon, ";");
        Some(())
    }

    fn expect_optional_semicolon(&mut self) -> Option<()> {
        if self.expect_optional_semicolon {
            let _ = self.p.expect(TokenKind::Semicolon, ";")?;
            return Some(());
        }
        Some(())
    }
}

impl<'ctx, 'pr> StmtParser<'ctx, 'pr> {
    pub fn parse_let(&mut self) -> Option<&'ctx Statement<'ctx>> {
        let start = self.p.current().span;
        let is_const = self.p.eat(TokenKind::Keyword(CompilerKeyword::Const));

        if !is_const {
            self.p
                .expect(TokenKind::Keyword(CompilerKeyword::Let), "let")?;
        }

        let name_token = self.p.expect(TokenKind::Ident, "identifier")?;
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

        dbg!(self.p.current());

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

    pub fn parse_defer(&mut self) -> Option<&'ctx Statement<'ctx>> {
        let defer_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Defer), "defer")?;

        let body = self.parse()?;

        let _ = self.p.eat(TokenKind::Semicolon);

        let stmt = self.p.arena.alloc(Statement {
            kind: StatementKind::Defer { body },
            span: body.merge_span(defer_kw.span),
        });

        Some(stmt)
    }

    pub fn parse_while(&mut self) -> Option<&'ctx Statement<'ctx>> {
        todo!()
    }

    pub fn parse_for(&mut self) -> Option<&'ctx Statement<'ctx>> {
        todo!()
    }

    pub fn parse_switch(&mut self) -> Option<&'ctx Statement<'ctx>> {
        todo!()
    }

    pub fn parse_block(&mut self) -> Option<&'ctx Statement<'ctx>> {
        todo!()
    }

    pub fn parse_expr_or_assign(&mut self) -> Option<&'ctx Statement<'ctx>> {
        let start = self.p.current().span;

        let lhs;

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

        if let Some(bin_info) = expressions::BinaryInfo::new(self.p.current()) {
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

        // expr in statement

        self.expect_optional_semicolon()?;

        let stmt = self.p.arena.alloc(Statement {
            kind: StatementKind::Expr(lhs),
            span: lhs.span,
        });

        Some(stmt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    use zeen_ast::{Expression, ExpressionKind};

    macro_rules! make_stmt_parser {
        ($src:expr, $tokens:ident, $bump:ident, $rodeo:ident, $parser:ident, $ep: ident) => {
            let src_arc = std::sync::Arc::new($src.to_string());
            let $rodeo = std::sync::Arc::new(std::sync::Mutex::new(lasso::Rodeo::default()));
            let $bump = bumpalo::Bump::new();
            let mut $tokens = zeen_lexer::tokenize($src);
            let mut $parser = Parser::new(
                "tests.zn",
                src_arc,
                &mut $tokens,
                &$bump,
                std::sync::Arc::clone(&$rodeo),
            );
            let mut $ep = StmtParser::new(&mut $parser);
        };
    }

    #[test]
    fn assign_basic() {
        const SRC: &str = "a = b";

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
    }

    #[test]
    fn assign_field() {
        const SRC: &str = "a.field = b";

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
    }

    #[test]
    fn assign_slice() {
        const SRC: &str = "a[123] = b";

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
    }

    #[test]
    fn assign_wtf() {
        const SRC: &str = "123 = a";

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
    }
}
