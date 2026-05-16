#![allow(unused)]

use bumpalo::Bump;
use lasso::Rodeo;
use smol_str::SmolStr;
use std::sync::Arc;

use error::ParserError;
use zeen_lexer::{Token, TokenKind};

pub mod error;
pub mod expressions;

pub struct Parser<'ctx> {
    src: Arc<String>,
    filename: &'ctx str,

    tokens: &'ctx mut dyn Iterator<Item = Token>,

    arena: &'ctx Bump,
    interner: &'ctx mut Rodeo,

    current: Option<Token>,
    peeked: Option<Token>,

    pub errors: Vec<ParserError>,
    panic_mode: bool,
}

impl<'ctx> Parser<'ctx> {
    pub fn new(
        filename: &'ctx str,
        src: Arc<String>,
        tokens: &'ctx mut dyn Iterator<Item = Token>,
        arena: &'ctx Bump,
        interner: &'ctx mut Rodeo,
    ) -> Self {
        let current = tokens.next();

        Self {
            filename,
            src,

            tokens,

            arena,
            interner,

            current,
            peeked: None,

            errors: Vec::new(),
            panic_mode: false,
        }
    }

    pub fn parse_program(
        &mut self,
    ) -> Result<&'ctx [zeen_ast::Declaration<'_>], &'ctx [ParserError]> {
        let mut decls: Vec<zeen_ast::Declaration> = Vec::new();

        while !self.is_eof() {
            todo!();
        }

        let arena_slice = self.arena.alloc_slice_clone(&decls);
        drop(decls);

        if self.errors.is_empty() {
            Ok(arena_slice)
        } else {
            Err(&self.errors)
        }
    }

    pub fn named_src(&self) -> miette::NamedSource<Arc<String>> {
        let src_ref = Arc::clone(&self.src);

        miette::NamedSource::new(self.filename, src_ref)
    }

    pub fn report(&mut self, err: ParserError) {
        self.errors.push(err);
        self.panic_mode = true;
    }

    pub fn current(&self) -> Option<&Token> {
        self.current.as_ref()
    }

    pub fn current_clone(&self) -> Option<Token> {
        self.current.clone()
    }

    pub fn at(&self, kind: TokenKind) -> bool {
        self.current().map_or(false, |token| token.kind == kind)
    }

    pub fn is_eof(&self) -> bool {
        self.current.is_none()
    }

    pub fn advance(&mut self) -> Option<Token> {
        let next = self.peeked.take().or_else(|| self.tokens.next());
        let prev = std::mem::replace(&mut self.current, next);

        if let Some(token) = &self.current {
            if token.kind == TokenKind::Unknown {
                self.report(ParserError::UnknownToken {
                    src: self.named_src(),
                    span: token.span,
                });
            }
        }

        prev
    }

    pub fn peek(&mut self) -> Option<&Token> {
        if self.peeked.is_none() {
            self.peeked = self.tokens.next();
        }
        self.peeked.as_ref()
    }

    pub fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    pub fn expect(&mut self, kind: TokenKind, display: &str) -> Result<Token, ()> {
        if self.at(kind) {
            Ok(self.advance().unwrap())
        } else {
            if let Some(cur) = self.current().clone() {
                self.report(ParserError::ExpectedToken {
                    expected: SmolStr::from(display),

                    src: self.named_src(),
                    span: cur.span,
                });
            } else {
                self.report(ParserError::UnexpectedEof {
                    expected: SmolStr::from(display),

                    src: self.named_src(),
                    span: (self.src.len() - 1, 0).into(),
                });
            }

            Err(())
        }
    }

    pub fn sync(&mut self) {
        self.panic_mode = false;

        loop {
            match self.current().map(|token| &token.kind) {
                None => break,

                Some(TokenKind::Semicolon) => {
                    let _ = self.advance();
                    break;
                }

                Some(TokenKind::Keyword(kw)) if is_sync_keyword(kw) => break,

                _ => {
                    self.advance();
                }
            }
        }
    }
}

fn is_sync_keyword(kw: &zeen_lexer::token::CompilerKeyword) -> bool {
    use zeen_lexer::token::CompilerKeyword::*;

    matches!(
        kw,
        If | While
            | For
            | Break
            | Let
            | Const
            | Defer
            | Switch
            | Return
            | Pub
            | Fn
            | Extern
            | Include
            | Link
            | Import
            | Struct
            | Enum
            | Interface
            | Implement
            | Type
    )
}
