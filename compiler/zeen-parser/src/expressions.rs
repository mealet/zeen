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

/// ==@ Expressions Parser @==

impl<'ctx, 'pr> ExprParser<'ctx, 'pr> {
    pub fn new(parser: &'pr mut Parser<'ctx>) -> Self {
        Self { p: parser }
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
        todo!()
    }

    fn parse_primary(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }
}

/// Literals Implementations
impl<'ctx, 'pr> ExprParser<'ctx, 'pr> {
    fn parse_literal_int(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_literal_float(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_literal_bool(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_literal_string(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_literal_null(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
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

    fn parse_call(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_macro_call(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_field_access(&mut self) -> Option<&'ctx Expression<'ctx>> {
        todo!()
    }

    fn parse_slice_access(&mut self) -> Option<&'ctx Expression<'ctx>> {
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
