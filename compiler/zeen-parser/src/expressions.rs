use crate::{Parser, error::ParserError};

use smallvec::SmallVec;
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
        '\'' => Some('\''),
        '"' => Some('"'),
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

        while self.p.current().kind != TokenKind::Eof {
            let current = self.p.current();

            let Some(op) = BinaryInfo::new(current) else {
                break;
            };

            if (op.prec as u8) < (min_prec as u8) {
                break;
            }

            if self.p.advance_not_eof().is_none() {
                break;
            };

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

        let token = self.p.current_clone();

        let op = match token.kind {
            TokenKind::Minus => UnaryOp::Neg,
            TokenKind::Bang => UnaryOp::Not,
            TokenKind::Tilde => UnaryOp::BitNot,
            TokenKind::Star => UnaryOp::Deref,
            TokenKind::Ampersand => UnaryOp::AddrOf,

            _ => return self.parse_postfix(),
        };

        let _ = self.p.advance_not_eof()?;

        let expr = self.parse_unary()?;

        let result = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Unary { expr, op },
            span: token.merge_span(expr.span),
        });

        Some(result)
    }

    fn parse_postfix(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let mut expr = self.parse_primary()?;

        loop {
            expr = match self.p.current().kind {
                TokenKind::OpenParen => self.parse_call(expr)?,
                TokenKind::OpenBracket => self.parse_slice_access(expr)?,
                TokenKind::OpenBrace => self.parse_struct_init_fields(expr)?,
                TokenKind::Dot => self.parse_field_access(expr)?,

                _ => break,
            }
        }

        Some(expr)
    }

    fn parse_primary(&mut self) -> Option<&'ctx Expression<'ctx>> {
        use zeen_lexer::token::CompilerKeyword;

        let token = self.p.current();

        match &token.kind {
            TokenKind::Literal { kind } => self.parse_literal(*kind),

            // --> keywords
            TokenKind::Keyword(CompilerKeyword::Null) => {
                // `null` literal is not included in `parse_literal` functions, but it is
                // written with the same rule: don't move cursor.

                let output = self.parse_literal_null();
                let _ = self.p.advance();

                output
            }

            TokenKind::Keyword(CompilerKeyword::True | CompilerKeyword::False) => {
                let output = self.parse_literal_bool();
                let _ = self.p.advance();

                output
            }

            TokenKind::Keyword(CompilerKeyword::If) => self.parse_if_expr(),

            TokenKind::Keyword(CompilerKeyword::SelfUpper | CompilerKeyword::SelfLower) => {
                self.parse_ident_or_struct_init()
            }

            // <-- keywords
            TokenKind::Ident => self.parse_ident_or_struct_init(),
            TokenKind::MacroIdent => self.parse_macro_call(),

            TokenKind::OpenParen => self.parse_grouped(),
            TokenKind::OpenBracket => self.parse_array_init(),
            TokenKind::OpenBrace => self.parse_block(),

            TokenKind::Eof => {
                self.p.report(ParserError::UnexpectedEof {
                    expected: "expression".into(),
                    src: self.p.named_src(),
                    span: token.span,
                });

                None
            }

            _ => {
                self.p.report(ParserError::UnknownExpression {
                    token_kind: format!("{:?}", token.kind).into(),
                    src: self.p.named_src(),
                    span: token.span,
                });

                None
            }
        }
    }
}

/// Literals Implementations
impl<'ctx, 'pr> ExprParser<'ctx, 'pr> {
    fn parse_literal(
        &mut self,
        literal: zeen_lexer::token::LiteralKind,
    ) -> Option<&'ctx Expression<'ctx>> {
        use zeen_lexer::token::LiteralKind;

        let token = self.p.current();

        let output = match literal {
            LiteralKind::Int { base } => self.parse_literal_int(base),
            LiteralKind::Float => self.parse_literal_float(),
            LiteralKind::Char { terminated, empty } => {
                self.parse_literal_char(false, terminated, empty)
            }
            LiteralKind::ByteChar { terminated, empty } => {
                self.parse_literal_char(true, terminated, empty)
            }
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

        let token = self.p.current();
        let span = token.span;

        let mut str_value =
            self.p.src[token.span.offset()..token.span.offset() + token.span.len()].to_owned();
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
                span,
            });

            0
        });

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Literal(expressions::Literal::Int(value)),
            span,
        });

        Some(expr)
    }

    fn parse_literal_float(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let token = self.p.current();
        let span = token.span;

        let mut str_value =
            self.p.src[token.span.offset()..token.span.offset() + token.span.len()].to_owned();

        let value = str_value.parse::<f64>().unwrap_or_else(|err| {
            self.p.report(ParserError::InvalidLiteral {
                message: "invalid float literal found".into(),
                label: format!("float parser returned: `{}`", err).into(),
                src: self.p.named_src(),
                span,
            });

            0.0
        });

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Literal(expressions::Literal::Float(value)),
            span,
        });

        Some(expr)
    }

    fn parse_literal_char(
        &mut self,
        is_byte: bool,
        terminated: bool,
        empty: bool,
    ) -> Option<&'ctx Expression<'ctx>> {
        let token = self.p.current_clone();

        if empty {
            self.p.report(ParserError::InvalidLiteral {
                message: "invalid char literal".into(),
                label: "char literal shouldn't be empty".into(),
                src: self.p.named_src(),
                span: token.span,
            });

            return None;
        }

        if !terminated {
            self.p.report(ParserError::InvalidLiteral {
                message: "invalid char literal".into(),
                label: "char literal is not closed".into(),
                src: self.p.named_src(),
                span: token.span,
            });

            return None;
        }

        let span = token.span;

        let byte_literal_offset = if is_byte { 1 } else { 0 };
        let str_value = (&self.p.src
            [token.span.offset() + byte_literal_offset..token.span.offset() + token.span.len()]);

        debug_assert_eq!(str_value.chars().nth(0), Some('\''));
        debug_assert_eq!(str_value.chars().last(), Some('\''));

        let inner_str = &str_value[1..str_value.len() - 1];

        let inner_value = match inner_str.len() {
            1 => inner_str.chars().nth(0).unwrap(),
            2 => {
                if let Some('\\') = inner_str.chars().nth(0) {
                    let escape = inner_str.chars().nth(1).unwrap();

                    character_escape(escape).unwrap_or_else(|| {
                        self.p.report(ParserError::InvalidCharacterEscape {
                            src: self.p.named_src(),
                            span,
                        });

                        ' '
                    })
                } else {
                    self.p.report(ParserError::InvalidLiteral {
                        message: "invalid char literal".into(),
                        label: "`char` literal must be a signle character".into(),
                        src: self.p.named_src(),
                        span,
                    });

                    ' '
                }
            }

            _ => {
                self.p.report(ParserError::InvalidLiteral {
                    message: "invalid char literal".into(),
                    label: "`char` literal must be a signle character".into(),
                    src: self.p.named_src(),
                    span,
                });

                ' '
            }
        };

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Literal(if is_byte {
                expressions::Literal::ByteChar(inner_value)
            } else {
                expressions::Literal::Char(inner_value)
            }),
            span: token.span,
        });

        Some(expr)
    }

    fn parse_literal_bool(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let token = self.p.current_clone();

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Literal(expressions::Literal::Bool(
                token.kind == TokenKind::Keyword(zeen_lexer::token::CompilerKeyword::True),
            )),
            span: token.span,
        });

        Some(expr)
    }

    fn parse_literal_string(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let token = self.p.current();
        let span = token.span;

        let token_slice =
            self.p.src[token.span.offset()..token.span.offset() + token.span.len()].to_owned();

        debug_assert_eq!(token_slice.chars().nth(0), Some('"'));
        debug_assert_eq!(token_slice.chars().last(), Some('"'));

        let raw_str = &token_slice[1..token_slice.len() - 1];

        let mut chars = raw_str.char_indices().peekable();
        let mut buffer = String::new();

        while let Some((pos, chr)) = chars.next() {
            if chr == '\\' {
                match chars.next() {
                    Some((_, next_chr)) => {
                        let escaped = character_escape(next_chr).unwrap_or_else(|| {
                            // TOKEN_OFFSET + 1 (for dquote) + inner offset
                            let error_offset = span.offset() + 1 + pos;

                            self.p.report(ParserError::InvalidCharacterEscape {
                                src: self.p.named_src(),
                                span: (error_offset, 2).into(),
                            });

                            ' '
                        });

                        buffer.push(escaped);
                    }

                    None => {
                        // TOKEN_OFFSET + 1 (for dquote) + inner offset
                        let error_offset = span.offset() + 1 + pos;

                        self.p.report(ParserError::InvalidCharacterEscape {
                            src: self.p.named_src(),
                            span: (error_offset, 2).into(),
                        });
                    }
                }
            } else {
                buffer.push(chr);
            }
        }

        let interned_id = self.p.get_or_intern(buffer);

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Literal(expressions::Literal::String(interned_id)),
            span,
        });

        Some(expr)
    }

    fn parse_literal_raw_string(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let token = self.p.current();
        let span = token.span;

        let token_slice =
            self.p.src[token.span.offset()..token.span.offset() + token.span.len()].to_owned();

        debug_assert_eq!(token_slice.chars().nth(0), Some('r'));
        debug_assert_eq!(token_slice.chars().last(), Some('#'));

        let raw_str = &token_slice["r#\"".len()..token_slice.len() - "\"#".len()];

        let interned_id = self.p.get_or_intern(raw_str);

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Literal(expressions::Literal::String(interned_id)),
            span,
        });

        Some(expr)
    }

    fn parse_literal_null(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let current = self.p.current();

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Literal(expressions::Literal::Null),
            span: current.span,
        });

        Some(expr)
    }
}

impl<'ctx, 'pr> ExprParser<'ctx, 'pr> {
    fn parse_ident_or_struct_init(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let token = self.p.current_clone();
        let token_slice =
            self.p.src[token.span.offset()..token.span.offset() + token.span.len()].to_string();

        let id = self.p.get_or_intern(token_slice);

        let mut generic_args: Option<&'_ [&'_ zeen_ast::TypeExpr<'_>]> = None;
        let mut span = token.span;

        let _ = self.p.advance();

        if self.p.eat(TokenKind::Hashtag) {
            let _ = self.p.expect(TokenKind::OpenBracket, "[")?;

            let mut args_buffer: SmallVec<[&zeen_ast::TypeExpr<'_>; 16]> = SmallVec::new();

            while self.p.current().kind != TokenKind::Eof {
                let current = self.p.current_clone();

                if self.p.eat(TokenKind::CloseBracket) {
                    span = token.merge_span(current.span);
                    break;
                }

                let mut type_parser = crate::type_parser::TypeParser::new(self.p);
                let generic_arg_type = type_parser.parse()?;

                args_buffer.push(generic_arg_type);

                let _ = self.p.eat(TokenKind::Comma);
            }

            let args_slice = self.p.arena.alloc_slice_clone(&args_buffer);
            drop(args_buffer);

            generic_args = Some(args_slice);
        }

        let base = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Ident {
                name: id,
                generic_args,
            },
            span,
        });

        if self.p.at(TokenKind::OpenBrace) {
            if token.kind == TokenKind::Keyword(zeen_lexer::token::CompilerKeyword::SelfLower) {
                self.p.report(ParserError::SyntaxError {
                    label: "unknown `self` struct init".into(),
                    help: Some("perhaps you wanted to use `Self` alias?".into()),
                    src: self.p.named_src(),
                    span: token.span,
                });

                return None;
            }

            return self.parse_struct_init_fields(base);
        }

        Some(base)
    }

    fn parse_struct_init_fields(
        &mut self,
        ty: &'ctx Expression<'ctx>,
    ) -> Option<&'ctx Expression<'ctx>> {
        use zeen_ast::expressions::FieldInit;
        assert!(self.p.eat(TokenKind::OpenBrace));

        let mut fields: Option<&'ctx [FieldInit<'ctx>]> = None;
        let mut last_span = self.p.current().span;

        if !self.p.eat(TokenKind::CloseBrace) {
            let mut fields_buffer: SmallVec<[FieldInit; 8]> = SmallVec::new();

            while !matches!(
                self.p.current().kind,
                TokenKind::CloseBrace | TokenKind::Eof
            ) {
                let dot_token = self.p.expect(TokenKind::Dot, ".")?;

                let identifier_token = self.p.expect(TokenKind::Ident, "identifier")?;
                let identifier_slice = self.p.src[identifier_token.span.offset()
                    ..identifier_token.span.offset() + identifier_token.span.len()]
                    .to_owned();
                let identifier_id = self.p.get_or_intern(identifier_slice);

                let _ = self.p.expect(TokenKind::Eq, "=")?;

                let value_expr = self.parse()?;
                let _ = self.p.eat(TokenKind::Comma);

                fields_buffer.push(FieldInit {
                    name: identifier_id,
                    value: value_expr,
                    span: value_expr.merge_span(dot_token.span),
                });
            }

            let close_brace = self.p.expect(TokenKind::CloseBrace, "}")?;

            let fields_arena = self.p.arena.alloc_slice_copy(&fields_buffer);
            drop(fields_buffer);

            fields = Some(fields_arena);
            last_span = close_brace.span;
        }

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::StructInit { ty, fields },
            span: ty.merge_span(last_span),
        });

        Some(expr)
    }

    fn parse_grouped(&mut self) -> Option<&'ctx Expression<'ctx>> {
        debug_assert!(self.p.at(TokenKind::OpenParen));

        let _ = self.p.advance_not_eof()?;
        let expr = self.parse();
        let _ = self.p.expect(TokenKind::CloseParen, ")")?;

        expr
    }

    fn parse_call(&mut self, callee: &'ctx Expression) -> Option<&'ctx Expression<'ctx>> {
        let macro_id: Option<lasso::Spur> = if let ExpressionKind::Macro(key) = callee.kind {
            Some(key)
        } else {
            None
        };

        let open_paren = self.p.expect(TokenKind::OpenParen, "(")?;
        let mut args_buffer: SmallVec<[&'ctx Expression<'ctx>; 12]> = SmallVec::new();

        if let Some(macro_id) = macro_id {
            let mut interner_lock = self.p.interner.lock().unwrap();
            let macro_name = interner_lock.resolve(&macro_id).to_owned();
            drop(interner_lock);

            if matches!(macro_name.as_ref(), "as!" | "sizeof!" | "alignof!") {
                let mut type_parser = crate::type_parser::TypeParser::new(self.p);

                let parsed_type = type_parser.parse()?;
                let type_expr = self.p.arena.alloc(Expression {
                    kind: ExpressionKind::Type(parsed_type),
                    span: parsed_type.span,
                });

                args_buffer.push(type_expr);
                let _ = self.p.eat(TokenKind::Comma);
            }
        }

        while !matches!(
            self.p.current().kind,
            TokenKind::CloseParen | TokenKind::Eof
        ) {
            let arg = self.parse()?;
            args_buffer.push(arg);

            if !self.p.at(TokenKind::CloseParen) {
                self.p.expect(TokenKind::Comma, ",");
            }
        }

        let close_paren = self.p.expect(TokenKind::CloseParen, ")")?;

        let args = self.p.arena.alloc_slice_copy(&args_buffer);
        drop(args_buffer);

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Call { callee, args },
            span: open_paren.merge_span(close_paren.span),
        });

        Some(expr)
    }

    fn parse_macro_call(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let macro_ident = self.p.expect(TokenKind::MacroIdent, "macro identifier")?;

        let ident_span = macro_ident.span;
        let ident_slice =
            self.p.src[ident_span.offset()..ident_span.offset() + ident_span.len()].to_owned();
        let ident_id = self.p.get_or_intern(ident_slice);

        let callee = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Macro(ident_id),
            span: macro_ident.span,
        });

        self.parse_call(callee)
    }

    fn parse_field_access(&mut self, object: &'ctx Expression) -> Option<&'ctx Expression<'ctx>> {
        let _ = self.p.advance_not_eof()?; // skip `.`

        let field_token = self.p.expect(TokenKind::Ident, "identifier")?;
        let field_span = field_token.span;
        let field_slice =
            self.p.src[field_span.offset()..field_span.offset() + field_span.len()].to_owned();

        let field_id = self.p.get_or_intern(field_slice);

        let mut generic_args: Option<&'_ [&'_ zeen_ast::TypeExpr<'_>]> = None;
        let mut span = field_token.span;

        if self.p.eat(TokenKind::Hashtag) {
            let _ = self.p.expect(TokenKind::OpenBracket, "[")?;

            let mut args_buffer: SmallVec<[&zeen_ast::TypeExpr<'_>; 16]> = SmallVec::new();

            while self.p.current().kind != TokenKind::Eof {
                let current = self.p.current_clone();

                if self.p.eat(TokenKind::CloseBracket) {
                    span = field_token.merge_span(current.span);
                    break;
                }

                let mut type_parser = crate::type_parser::TypeParser::new(self.p);
                let generic_arg_type = type_parser.parse()?;

                args_buffer.push(generic_arg_type);

                let _ = self.p.eat(TokenKind::Comma);
            }

            let args_slice = self.p.arena.alloc_slice_clone(&args_buffer);
            drop(args_buffer);

            generic_args = Some(args_slice);
        }

        let ident_expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::Ident {
                name: field_id,
                generic_args,
            },
            span: field_token.span,
        });

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::FieldAccess {
                object,
                field: ident_expr,
            },
            span: object.merge_span(field_token.span),
        });

        Some(expr)
    }

    fn parse_slice_access(&mut self, object: &'ctx Expression) -> Option<&'ctx Expression<'ctx>> {
        let _ = self.p.advance_not_eof()?;
        let index = self.parse()?;
        let close_bracket = self.p.expect(TokenKind::CloseBracket, "]")?;

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::SliceAccess { object, index },
            span: object.merge_span(close_bracket.span),
        });

        Some(expr)
    }

    fn parse_if_expr(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let if_kw = self.p.expect(
            TokenKind::Keyword(zeen_lexer::token::CompilerKeyword::If),
            "if",
        )?;

        let condition = self.parse_grouped()?;
        let then_block = self.parse()?;

        let mut else_block: Option<&'ctx Expression<'ctx>> = None;

        if self
            .p
            .eat(TokenKind::Keyword(zeen_lexer::token::CompilerKeyword::Else))
        {
            else_block = Some(self.parse()?);
        }

        let span = if let Some(expr) = else_block {
            if_kw.merge_span(expr.span)
        } else {
            if_kw.merge_span(then_block.span)
        };

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::If {
                condition,
                then_block,
                else_block,
            },
            span,
        });

        Some(expr)
    }

    fn parse_array_init(&mut self) -> Option<&'ctx Expression<'ctx>> {
        let open = self.p.expect(TokenKind::OpenBracket, "[")?;

        let mut elements_buffer: SmallVec<[&'ctx Expression; 8]> = SmallVec::new();

        while !matches!(
            self.p.current.kind,
            TokenKind::CloseBracket | TokenKind::Eof
        ) {
            let expr = self.parse()?;
            elements_buffer.push(expr);

            if !self.p.at(TokenKind::CloseBracket) {
                self.p.expect(TokenKind::Comma, ",");
            }
        }

        let close = self.p.expect(TokenKind::CloseBracket, "]")?;
        let elements = self.p.arena.alloc_slice_copy(&elements_buffer);

        let expr = self.p.arena.alloc(Expression {
            kind: ExpressionKind::ArrayInit { elements },
            span: open.merge_span(close.span),
        });

        Some(expr)
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

    macro_rules! make_expr_parser {
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
            let mut $ep = ExprParser::new(&mut $parser);
        };
    }

    #[test]
    fn literal_int() {
        const SRC: &str = "123 0x1ef 0b1011 0o123";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        {
            let expr = expr_parser.parse().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Int(123)),
                    span: (0, 3).into()
                }
            );
        }

        {
            let expr = expr_parser.parse().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Int(0x1ef)),
                    span: (4, 5).into()
                }
            );
        }

        {
            let expr = expr_parser.parse().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Int(0b1011)),
                    span: (10, 6).into()
                }
            );
        }

        {
            let expr = expr_parser.parse().unwrap();

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
    #[allow(clippy::approx_constant, clippy::excessive_precision)]
    fn literal_float() {
        const SRC: &str = "1.0 3.1415926535897932384626";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        {
            let expr = expr_parser.parse().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Float(1.0)),
                    span: (0, 3).into()
                }
            );
        }

        {
            let expr = expr_parser.parse().unwrap();

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

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        {
            let expr = expr_parser.parse().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Char('a')),
                    span: (0, 3).into()
                }
            );
        }

        {
            let expr = expr_parser.parse().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Char('\0')),
                    span: (4, 4).into()
                }
            );
        }

        {
            let expr = expr_parser.parse().unwrap();

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

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        {
            let expr = expr_parser.parse().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::ByteChar('a')),
                    span: (0, 4).into()
                }
            );
        }

        {
            let expr = expr_parser.parse().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::ByteChar('\0')),
                    span: (5, 5).into()
                }
            );
        }

        {
            let expr = expr_parser.parse().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::ByteChar('\\')),
                    span: (11, 5).into()
                }
            );
        }
    }

    #[test]
    fn literal_bool() {
        const SRC: &str = "true false";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        {
            let expr = expr_parser.parse().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Bool(true)),
                    span: (0, 4).into()
                }
            );
        }

        {
            let expr = expr_parser.parse().unwrap();

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

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        {
            let expr = expr_parser.parse().unwrap();
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
            let expr = expr_parser.parse().unwrap();
            let id = rodeo.lock().unwrap().get("new line \n").unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::String(id)),
                    span: (15, 13).into()
                }
            );
        }
    }

    #[test]
    fn literal_raw_string() {
        const SRC: &str = "r#\"hello \\n \\0\"#";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        {
            let expr = expr_parser.parse().unwrap();
            let id = rodeo.lock().unwrap().get("hello \\n \\0").unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::String(id)),
                    span: (0, 16).into()
                }
            );
        }
    }

    #[test]
    fn literal_null() {
        const SRC: &str = "null";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        {
            let expr = expr_parser.parse().unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Literal(expressions::Literal::Null),
                    span: (0, "null".len()).into()
                }
            );
        }
    }

    #[test]
    fn ident_simple() {
        const SRC: &str = "foo";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        {
            let expr = expr_parser.parse().unwrap();
            let spur = rodeo.lock().unwrap().try_get_or_intern("foo").unwrap();

            assert_eq!(
                expr,
                &Expression {
                    kind: ExpressionKind::Ident {
                        name: spur,
                        generic_args: None,
                    },
                    span: (0, 3).into()
                }
            );
        }
    }

    #[test]
    fn ident_with_generics() {
        use zeen_ast::types::*;

        const SRC: &str = "foo#[i32, u32]";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert_eq!(
            expr_parser.parse().unwrap(),
            &Expression {
                kind: ExpressionKind::Ident {
                    name: { rodeo.lock().unwrap().get_or_intern("foo") },
                    generic_args: Some(&[
                        &TypeExpr {
                            kind: TypeKind::Builtin(BuiltinType::i32),
                            span: (5, 3).into(),
                        },
                        &TypeExpr {
                            kind: TypeKind::Builtin(BuiltinType::u32),
                            span: (10, 3).into(),
                        },
                    ]),
                },
                span: (0, 14).into()
            }
        );
    }

    #[test]
    fn struct_init_empty() {
        use zeen_ast::types::*;

        const SRC: &str = "foo {}";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert_eq!(
            expr_parser.parse().unwrap(),
            &Expression {
                kind: ExpressionKind::StructInit {
                    ty: &Expression {
                        kind: ExpressionKind::Ident {
                            name: { rodeo.lock().unwrap().get_or_intern("foo") },
                            generic_args: None,
                        },
                        span: (0, 3).into()
                    },
                    fields: None,
                },
                span: (0, 6).into()
            }
        );
    }

    #[test]
    fn struct_init_with_generic() {
        use zeen_ast::types::*;

        const SRC: &str = "foo#[i32] {}";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert_eq!(
            expr_parser.parse().unwrap(),
            &Expression {
                kind: ExpressionKind::StructInit {
                    ty: &Expression {
                        kind: ExpressionKind::Ident {
                            name: { rodeo.lock().unwrap().get_or_intern("foo") },
                            generic_args: Some(&[
                                // fmt comment
                                &TypeExpr {
                                    kind: TypeKind::Builtin(BuiltinType::i32),
                                    span: (5, 3).into()
                                }
                            ]),
                        },
                        span: (0, 9).into()
                    },
                    fields: None,
                },
                span: (0, 12).into()
            }
        );
    }

    #[test]
    fn struct_init_with_field() {
        use zeen_ast::expressions::FieldInit;

        const SRC: &str = "foo { .a = 123 }";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert_eq!(
            expr_parser.parse().unwrap(),
            &Expression {
                kind: ExpressionKind::StructInit {
                    ty: &Expression {
                        kind: ExpressionKind::Ident {
                            name: { rodeo.lock().unwrap().get_or_intern("foo") },
                            generic_args: None,
                        },
                        span: (0, 3).into()
                    },
                    fields: Some(&[
                        // fmt comment
                        FieldInit {
                            name: { rodeo.lock().unwrap().get_or_intern("a") },
                            value: &Expression {
                                kind: ExpressionKind::Literal(expressions::Literal::Int(123)),
                                span: (11, 3).into(),
                            },
                            span: (6, 8).into(),
                        }
                    ]),
                },
                span: (0, 16).into()
            }
        );
    }

    #[test]
    fn grouped_expr() {
        const SRC: &str = "(1 + 1)";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert_eq!(expr_parser.parse().unwrap().span, (1, 5).into());
    }

    #[test]
    fn call_empty() {
        const SRC: &str = "foo()";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert_eq!(
            expr_parser.parse().unwrap(),
            &Expression {
                kind: ExpressionKind::Call {
                    callee: &Expression {
                        kind: ExpressionKind::Ident {
                            name: { rodeo.lock().unwrap().get_or_intern("foo") },
                            generic_args: None,
                        },
                        span: (0, 3).into(),
                    },
                    args: &[],
                },
                span: (3, 2).into()
            }
        );
    }

    #[test]
    fn call_with_generic() {
        use zeen_ast::types::*;

        const SRC: &str = "foo#[i32]()";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert_eq!(
            expr_parser.parse().unwrap(),
            &Expression {
                kind: ExpressionKind::Call {
                    callee: &Expression {
                        kind: ExpressionKind::Ident {
                            name: { rodeo.lock().unwrap().get_or_intern("foo") },
                            generic_args: Some(&[&TypeExpr {
                                kind: TypeKind::Builtin(BuiltinType::i32),
                                span: (5, 3).into()
                            }]),
                        },
                        span: (0, 9).into(),
                    },
                    args: &[],
                },
                span: (9, 2).into()
            }
        );
    }

    #[test]
    fn call_with_args() {
        const SRC: &str = "foo(123, 321)";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert_eq!(
            expr_parser.parse().unwrap(),
            &Expression {
                kind: ExpressionKind::Call {
                    callee: &Expression {
                        kind: ExpressionKind::Ident {
                            name: { rodeo.lock().unwrap().get_or_intern("foo") },
                            generic_args: None,
                        },
                        span: (0, 3).into(),
                    },
                    args: &[
                        &Expression {
                            kind: ExpressionKind::Literal(expressions::Literal::Int(123)),
                            span: (4, 3).into(),
                        },
                        &Expression {
                            kind: ExpressionKind::Literal(expressions::Literal::Int(321)),
                            span: (9, 3).into(),
                        },
                    ],
                },
                span: (3, 10).into()
            }
        );
    }

    #[test]
    fn basic_macro_call() {
        // NOTE: In this case we're just assuming that it parses

        const SRC: &str = "foo!(123, 321)";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert!(expr_parser.parse().is_some());
    }

    #[test]
    fn type_required_macro_call() {
        // NOTE: In this case we're just assuming that it parses

        const SRC: &str = "as!(*const i32, 123) sizeof!([]void) alignof!(some_struct)";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert!(expr_parser.parse().is_some());
        assert!(expr_parser.parse().is_some());
        assert!(expr_parser.parse().is_some());

        assert!(expr_parser.parse().is_none());
    }

    #[test]
    fn field_access() {
        // NOTE: In this case we're just assuming that it parses

        const SRC: &str = "field.with_generic#[i32].lets_init_struct { .a = 123 } .and_call_fn()";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert!(expr_parser.parse().is_some());

        assert!(expr_parser.parse().is_none());
    }

    #[test]
    fn if_expr() {
        // NOTE: In this case we're just assuming that it parses

        const SRC: &str = "if (1 == 1) 123";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert!(expr_parser.parse().is_some());

        assert!(expr_parser.parse().is_none());
    }

    #[test]
    fn if_else_expr() {
        // NOTE: In this case we're just assuming that it parses

        const SRC: &str = "if (1 == 1) 123 else 321";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert!(expr_parser.parse().is_some());

        assert!(expr_parser.parse().is_none());
    }

    #[test]
    #[should_panic]
    fn if_without_parentheses() {
        // NOTE: In this case we're just assuming that it parses

        const SRC: &str = "if 1 == 1 123";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert!(expr_parser.parse().is_some());

        assert!(expr_parser.parse().is_none());
    }

    #[test]
    #[should_panic]
    fn if_without_then() {
        // NOTE: In this case we're just assuming that it parses

        const SRC: &str = "if (1 == 1) ";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert!(expr_parser.parse().is_some());

        assert!(expr_parser.parse().is_none());
    }

    #[test]
    #[should_panic]
    fn if_else_without_expr() {
        // NOTE: In this case we're just assuming that it parses

        const SRC: &str = "if (1 == 1) 123 else";

        make_expr_parser!(SRC, tokens, bump, rodeo, parser, expr_parser);

        assert!(expr_parser.parse().is_some());

        assert!(expr_parser.parse().is_none());
    }
}
