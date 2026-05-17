use crate::{Parser, error::ParserError};
use strum::FromRepr;
use zeen_ast::expressions::{self, Expression, ExpressionKind};
use zeen_lexer::{Token, TokenKind};

pub struct ExprParser<'ctx, 'pr> {
    p: &'pr mut Parser<'ctx>,
}

#[repr(u8)]
#[derive(PartialEq, Eq, PartialOrd, Ord, FromRepr, Copy, Clone)]
enum Precedence {
    Lowest,
    LogicalOr,
    LogicalAnd,
    BitwiseOr,
    BitwiseXor,
    BitwiseAnd,
    Equality,
    Comparison,
    Shift,
    Additive,
    Multiplicative,
    NonBinary,
}

impl Precedence {
    pub fn next(self) -> Self {
        let next_val = (self as u8).saturating_add(1);
        let max_allowed = Precedence::Multiplicative as u8;

        Precedence::from_repr(next_val.min(max_allowed)).unwrap()
    }
}

struct BinaryInfo {
    tag: expressions::BinaryOp,
    prec: Precedence,
}

impl BinaryInfo {
    pub fn new(token: &Token) -> Option<Self> {
        use TokenKind::*;
        use expressions::BinaryOp;

        match token.kind {
            Plus => Some(Self {
                tag: BinaryOp::Add,
                prec: Precedence::Additive,
            }),

            Minus => Some(Self {
                tag: BinaryOp::Sub,
                prec: Precedence::Additive,
            }),

            Star => Some(Self {
                tag: BinaryOp::Mul,
                prec: Precedence::Multiplicative,
            }),

            Slash => Some(Self {
                tag: BinaryOp::Div,
                prec: Precedence::Multiplicative,
            }),

            Percent => Some(Self {
                tag: BinaryOp::Mod,
                prec: Precedence::Multiplicative,
            }),

            BooleanEq => Some(Self {
                tag: BinaryOp::Eq,
                prec: Precedence::Equality,
            }),

            BooleanNe => Some(Self {
                tag: BinaryOp::Ne,
                prec: Precedence::Equality,
            }),

            Lt => Some(Self {
                tag: BinaryOp::Lt,
                prec: Precedence::Comparison,
            }),

            Gt => Some(Self {
                tag: BinaryOp::Gt,
                prec: Precedence::Comparison,
            }),

            Leq => Some(Self {
                tag: BinaryOp::Le,
                prec: Precedence::Comparison,
            }),

            Geq => Some(Self {
                tag: BinaryOp::Ge,
                prec: Precedence::Comparison,
            }),

            BooleanAnd => Some(Self {
                tag: BinaryOp::LogicalAnd,
                prec: Precedence::LogicalAnd,
            }),

            BooleanOr => Some(Self {
                tag: BinaryOp::LogicalOr,
                prec: Precedence::LogicalOr,
            }),

            Ampersand => Some(Self {
                tag: BinaryOp::BitAnd,
                prec: Precedence::BitwiseAnd,
            }),

            Pipe => Some(Self {
                tag: BinaryOp::BitOr,
                prec: Precedence::BitwiseOr,
            }),

            Caret => Some(Self {
                tag: BinaryOp::BitXor,
                prec: Precedence::BitwiseXor,
            }),

            LShift => Some(Self {
                tag: BinaryOp::Shl,
                prec: Precedence::Shift,
            }),

            RShift => Some(Self {
                tag: BinaryOp::Shr,
                prec: Precedence::Shift,
            }),

            _ => None,
        }
    }
}

fn character_escape(escape: char) -> Option<char> {
    match escape {
        '0' => Some('\0'),
        'n' => Some('\n'),
        'r' => Some('\r'),
        't' => Some('\t'),
        '\\' => Some('\\'),
        _ => None,
    }
}

/// ==@ Expressions Parser @==

impl<'ctx, 'pr> ExprParser<'ctx, 'pr> {
    pub fn new(parser: &'pr mut Parser<'ctx>) -> Self {
        Self { p: parser }
    }

    pub fn errors(&self) -> &[ParserError] {
        &self.p.errors
    }

    pub fn parse(&mut self) -> Option<&'ctx Expression<'ctx>> {
        self.parse_precedence(Precedence::Lowest)
    }

    pub fn parse_non_binary(&mut self) -> Option<&'ctx Expression<'ctx>> {
        self.parse_precedence(Precedence::NonBinary)
    }

    fn parse_precedence(&mut self, min_prec: Precedence) -> Option<&'ctx Expression<'ctx>> {
        let mut lhs = self.parse_unary()?;

        loop {
            let Some(current) = self.p.current() else {
                break;
            };
            let Some(op) = BinaryInfo::new(current) else {
                break;
            };

            if (op.prec as u8) < (min_prec as u8) {
                break;
            }

            let _ = self.p.advance()?;

            let rhs = self.parse_precedence(op.prec.next())?;

            lhs = self.p.arena.alloc(Expression {
                kind: ExpressionKind::Binary {
                    lhs,
                    rhs,
                    op: op.tag,
                },
                span: lhs.merge_span(rhs.span),
            });
        }

        Some(lhs)
    }

    fn parse_unary(&mut self) -> Option<&'ctx Expression<'ctx>> {
        use expressions::UnaryOp;

        let token = self.p.current_clone()?;

        let op = match token.kind {
            TokenKind::Minus => UnaryOp::Neg,
            TokenKind::Bang => UnaryOp::Not,
            TokenKind::Tilde => UnaryOp::BitNot,
            TokenKind::Star => UnaryOp::Deref,
            TokenKind::Ampersand => UnaryOp::AddrOf,

            _ => return self.parse_postfix(),
        };

        let _ = self.p.advance()?;

        let expr = self.parse_unary()?;

        let result = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Unary { expr, op },
            span: token.merge_span(expr.span),
        });

        return Some(result);
    }

    fn parse_postfix(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let mut expr = self.parse_primary()?;

        loop {
            expr = match self.p.current()?.kind {
                TokenKind::OpenParen => self.parse_call(expr)?,
                TokenKind::OpenBracket => self.parse_slice_access(expr)?,
                TokenKind::Dot => self.parse_field_access(expr)?,

                _ => break,
            }
        }

        Some(expr)
    }

    fn parse_primary(&mut self) -> Option<&'ctx Expression<'ctx>> {
        use zeen_lexer::token::CompilerKeyword;

        let token = self.p.current()?;

        return match &token.kind {
            TokenKind::Literal { kind } => self.parse_literal(*kind),
            TokenKind::Keyword(kw) => match kw {
                CompilerKeyword::Null => {
                    // `null` literal is not included in `parse_literal` functions, but it is
                    // written with the same rule: don't move cursor.

                    let output = self.parse_literal_null();
                    let _ = self.p.advance();

                    output
                }
                _ => todo!(),
            },

            _ => {
                self.p.report(ParserError::UnknownExpression {
                    token_kind: format!("{:?}", token.kind).into(),
                    src: self.p.named_src(),
                    span: token.span,
                });

                None
            }
        };
    }
}

/// Literals Implementations
impl<'ctx, 'pr> ExprParser<'ctx, 'pr> {
    fn parse_literal(
        &mut self,
        literal: zeen_lexer::token::LiteralKind,
    ) -> Option<&'ctx Expression<'ctx>> {
        use zeen_lexer::token::LiteralKind;

        let token = self.p.current()?;

        let output = match literal {
            LiteralKind::Int { base } => self.parse_literal_int(base),
            LiteralKind::Float => self.parse_literal_float(),
            LiteralKind::Char { terminated, empty } => self.parse_literal_char(),
            LiteralKind::ByteChar { terminated, empty } => self.parse_literal_bytechar(),
            LiteralKind::Str { terminated } => self.parse_literal_string(),
            LiteralKind::RawStr { terminated } => self.parse_literal_raw_string(),
            LiteralKind::InvalidRawStr => {
                self.p.report(ParserError::InvalidLiteral {
                    message: "invalid raw string literal found".into(),
                    label: "verify this literal".into(),
                    src: self.p.named_src(),
                    span: token.span,
                });

                None
            }
        };

        let _ = self.p.advance();

        output
    }

    fn parse_literal_int(
        &mut self,
        base: zeen_lexer::token::IntBase,
    ) -> Option<&'ctx Expression<'ctx>> {
        use zeen_lexer::token::IntBase;

        let token = self.p.current()?;
        let span = token.span;

        let mut str_value =
            (&self.p.src[token.span.offset()..token.span.offset() + token.span.len()]).to_owned();
        let radix = base as u32;

        match base {
            IntBase::Binary => str_value = str_value.replace("0b", ""),
            IntBase::Hexadecimal => str_value = str_value.replace("0x", ""),
            IntBase::Octal => str_value = str_value.replace("0o", ""),
            IntBase::Decimal => {}
        };

        let value = i64::from_str_radix(&str_value, radix).unwrap_or_else(|err| {
            self.p.report(ParserError::InvalidLiteral {
                message: "invalid integer literal found".into(),
                label: format!("number parser returned: `{}`", err).into(),
                src: self.p.named_src(),
                span: span,
            });

            return 0;
        });

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Literal(expressions::Literal::Int(value)),
            span,
        });

        Some(expr)
    }

    fn parse_literal_float(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let token = self.p.current()?;
        let span = token.span;

        let mut str_value =
            (&self.p.src[token.span.offset()..token.span.offset() + token.span.len()]).to_owned();

        let value = str_value.parse::<f64>().unwrap_or_else(|err| {
            self.p.report(ParserError::InvalidLiteral {
                message: "invalid float literal found".into(),
                label: format!("float parser returned: `{}`", err).into(),
                src: self.p.named_src(),
                span: span,
            });

            return 0.0;
        });

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Literal(expressions::Literal::Float(value)),
            span,
        });

        Some(expr)
    }

    fn parse_literal_char(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let token = self.p.current_clone()?;
        let span = token.span;

        let mut str_value =
            (&self.p.src[token.span.offset()..token.span.offset() + token.span.len()]).to_owned();

        debug_assert_eq!(str_value.chars().nth(0), Some('\''));
        debug_assert_eq!(str_value.chars().last(), Some('\''));

        let inner_str = &str_value[1..str_value.len() - 1];

        let inner_value = match inner_str.len() {
            1 => inner_str.chars().nth(0).unwrap(),
            2 => {
                if let Some('\\') = inner_str.chars().nth(0) {
                    let escape = inner_str.chars().nth(1).unwrap();

                    character_escape(escape).unwrap_or_else(|| {
                        self.p.report(ParserError::InvalidLiteral {
                            message: "invalid char literal".into(),
                            label: "this character escape is invalid".into(),
                            src: self.p.named_src(),
                            span: span,
                        });

                        ' '
                    })
                } else {
                    self.p.report(ParserError::InvalidLiteral {
                        message: "invalid char literal".into(),
                        label: "`char` literal must be a signle character".into(),
                        src: self.p.named_src(),
                        span: span,
                    });

                    ' '
                }
            }

            _ => {
                self.p.report(ParserError::InvalidLiteral {
                    message: "invalid char literal".into(),
                    label: "`char` literal must be a signle character".into(),
                    src: self.p.named_src(),
                    span: span,
                });

                ' '
            }
        };

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Literal(expressions::Literal::Char(inner_value)),
            span: token.span,
        });

        Some(expr)
    }

    fn parse_literal_bytechar(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_literal_bool(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_literal_string(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_literal_raw_string(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_literal_null(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let current = self.p.current()?;

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Literal(expressions::Literal::Null),
            span: current.span,
        });

        return Some(expr);
    }
}

impl<'ctx, 'pr> ExprParser<'ctx, 'pr> {
    fn parse_ident_or_struct_init(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_struct_init_fields(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_grouped(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_call(&mut self, callee: &Expression) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_macro_call(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_field_access(&mut self, object: &Expression) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_slice_access(&mut self, object: &Expression) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_if_expr(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_array_init(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_block(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }
}

impl<'ctx, 'pr> ExprParser<'ctx, 'pr> {
    fn parse_switch(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_int() {
        const SRC: &str = "123 0x1ef 0b1011 0o123";

        let src = std::sync::Arc::new(SRC.to_string());

        let rodeo = std::sync::Arc::new(std::sync::Mutex::new(lasso::Rodeo::default()));
        let bump = bumpalo::Bump::new();

        let mut tokens = zeen_lexer::tokenize(SRC);
        let mut parser = Parser::new(
            "tests.zn",
            src,
            &mut tokens,
            &bump,
            std::sync::Arc::clone(&rodeo),
        );

        let mut expr_parser = ExprParser::new(&mut parser);

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Int(123)),
                    span: (0, 3).into()
                }
            );
        }

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Int(0x1ef)),
                    span: (4, 5).into()
                }
            );
        }

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Int(0b1011)),
                    span: (10, 6).into()
                }
            );
        }

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Int(0o123)),
                    span: (17, 5).into()
                }
            );
        }
    }

    #[test]
    fn literal_float() {
        const SRC: &str = "1.0 3.1415926535897932384626";

        let src = std::sync::Arc::new(SRC.to_string());

        let rodeo = std::sync::Arc::new(std::sync::Mutex::new(lasso::Rodeo::default()));
        let bump = bumpalo::Bump::new();

        let mut tokens = zeen_lexer::tokenize(SRC);
        let mut parser = Parser::new(
            "tests.zn",
            src,
            &mut tokens,
            &bump,
            std::sync::Arc::clone(&rodeo),
        );

        let mut expr_parser = ExprParser::new(&mut parser);

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Float(1.0)),
                    span: (0, 3).into()
                }
            );
        }

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Float(
                        3.1415926535897932384626
                    )),
                    span: (4, 24).into()
                }
            );
        }
    }

    #[test]
    fn literal_char() {
        const SRC: &str = "'a' '\\0' '\\\\'";

        let src = std::sync::Arc::new(SRC.to_string());

        let rodeo = std::sync::Arc::new(std::sync::Mutex::new(lasso::Rodeo::default()));
        let bump = bumpalo::Bump::new();

        let mut tokens = zeen_lexer::tokenize(SRC);
        let mut parser = Parser::new(
            "tests.zn",
            src,
            &mut tokens,
            &bump,
            std::sync::Arc::clone(&rodeo),
        );

        let mut expr_parser = ExprParser::new(&mut parser);

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Char('a')),
                    span: (0, 3).into()
                }
            );
        }

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Char('\0')),
                    span: (4, 4).into()
                }
            );
        }

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Char('\\')),
                    span: (9, 4).into()
                }
            );
        }
    }

    #[test]
    fn literal_bytechar() {
        const SRC: &str = "b'a' b'\\0' b'\\\\'";

        let src = std::sync::Arc::new(SRC.to_string());

        let rodeo = std::sync::Arc::new(std::sync::Mutex::new(lasso::Rodeo::default()));
        let bump = bumpalo::Bump::new();

        let mut tokens = zeen_lexer::tokenize(SRC);
        let mut parser = Parser::new(
            "tests.zn",
            src,
            &mut tokens,
            &bump,
            std::sync::Arc::clone(&rodeo),
        );

        let mut expr_parser = ExprParser::new(&mut parser);

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::ByteChar('a')),
                    span: (0, 4).into()
                }
            );
        }

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::ByteChar('\0')),
                    span: (5, 5).into()
                }
            );
        }

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::ByteChar('\\')),
                    span: (10, 5).into()
                }
            );
        }
    }

    #[test]
    fn literal_bool() {
        const SRC: &str = "true false";

        let src = std::sync::Arc::new(SRC.to_string());

        let rodeo = std::sync::Arc::new(std::sync::Mutex::new(lasso::Rodeo::default()));
        let bump = bumpalo::Bump::new();

        let mut tokens = zeen_lexer::tokenize(SRC);
        let mut parser = Parser::new(
            "tests.zn",
            src,
            &mut tokens,
            &bump,
            std::sync::Arc::clone(&rodeo),
        );

        let mut expr_parser = ExprParser::new(&mut parser);

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Bool(true)),
                    span: (0, 4).into()
                }
            );
        }

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Bool(false)),
                    span: (5, 5).into()
                }
            );
        }
    }

    #[test]
    fn literal_string() {
        const SRC: &str = "\"hello, world\" \"new line \\n\"";

        let src = std::sync::Arc::new(SRC.to_string());

        let rodeo = std::sync::Arc::new(std::sync::Mutex::new(lasso::Rodeo::default()));
        let bump = bumpalo::Bump::new();

        let mut tokens = zeen_lexer::tokenize(SRC);
        let mut parser = Parser::new(
            "tests.zn",
            src,
            &mut tokens,
            &bump,
            std::sync::Arc::clone(&rodeo),
        );

        let mut expr_parser = ExprParser::new(&mut parser);

        {
            let expr = expr_parser.parse_primary().unwrap();
            let id = rodeo.lock().unwrap().get("hello, world").unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::String(id)),
                    span: (0, "hello, world".len() + 2).into()
                }
            );
        }

        {
            let expr = expr_parser.parse_primary().unwrap();
            let id = rodeo.lock().unwrap().get("new line \n").unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::String(id)),
                    span: (0, 13).into()
                }
            );
        }
    }

    #[test]
    fn literal_null() {
        const SRC: &str = "null";

        let src = std::sync::Arc::new(SRC.to_string());

        let rodeo = std::sync::Arc::new(std::sync::Mutex::new(lasso::Rodeo::default()));
        let bump = bumpalo::Bump::new();

        let mut tokens = zeen_lexer::tokenize(SRC);
        let mut parser = Parser::new(
            "tests.zn",
            src,
            &mut tokens,
            &bump,
            std::sync::Arc::clone(&rodeo),
        );

        let mut expr_parser = ExprParser::new(&mut parser);

        {
            let expr = expr_parser.parse_primary().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Null),
                    span: (0, "null".len()).into()
                }
            );
        }
    }
}
