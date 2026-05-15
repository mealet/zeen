use crate::{Parser, error::ParserError};
use strum::FromRepr;
use zeen_ast::expressions::{self, Expression, ExpressionKind};
use zeen_lexer::{Token, TokenKind};

pub struct ExprParser<'ctx> {
    p: &'ctx Parser<'ctx>,
}

#[repr(u8)]
#[derive(PartialEq, Eq, PartialOrd, Ord, FromRepr)]
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

// ==@ Expressions Parser @==

impl<'ctx> ExprParser<'ctx> {
    pub fn new(parser: &'ctx Parser<'ctx>) -> Self {
        Self { p: parser }
    }

    pub fn parse(&mut self) -> Option<Expression<'_>> {
        self.parse_precedence(Precedence::Lowest)
    }

    pub fn parse_non_binary(&mut self) -> Option<Expression<'_>> {
        self.parse_precedence(Precedence::NonBinary)
    }

    fn parse_precedence(&mut self, min_prec: Precedence) -> Option<Expression<'_>> {
        todo!()
    }
}
