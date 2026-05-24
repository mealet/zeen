use crate::{Parser, error::ParserError};

use smallvec::SmallVec;

use zeen_ast::statements::{self, Statement, StatementKind};
use zeen_lexer::{Token, TokenKind, token::CompilerKeyword};

pub struct StmtParser<'ctx, 'pr> {
    p: &'pr mut Parser<'ctx>,
}

/// ==@ Statements Parser @==
impl<'ctx, 'pr> StmtParser<'ctx, 'pr> {
    pub fn new(parser: &'pr mut Parser<'ctx>) -> Self {
        Self { p: parser }
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
}

impl<'ctx, 'pr> StmtParser<'ctx, 'pr> {
    pub fn parse_let(&mut self) -> Option<&'ctx Statement<'ctx>> {
        todo!()
    }

    pub fn parse_return(&mut self) -> Option<&'ctx Statement<'ctx>> {
        todo!()
    }

    pub fn parse_break(&mut self) -> Option<&'ctx Statement<'ctx>> {
        todo!()
    }

    pub fn parse_defer(&mut self) -> Option<&'ctx Statement<'ctx>> {
        todo!()
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
        todo!()
    }
}
