#![allow(unused)]

use bumpalo::Bump;
use lasso::Rodeo;
use smol_str::SmolStr;
use std::sync::{Arc, Mutex};

use error::ParserError;
use zeen_lexer::{Token, TokenKind};

pub mod error;
pub mod expressions;
pub mod type_parser;

pub struct Parser<'ctx> {
    src: Arc<String>,
    filename: &'ctx str,

    tokens: &'ctx mut dyn Iterator<Item = Token>,

    arena: &'ctx Bump,
    interner: Arc<Mutex<Rodeo>>,

    current: Token,
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
        interner: Arc<Mutex<Rodeo>>,
    ) -> Self {
        let current = tokens
            .next()
            .unwrap_or(Token::new(TokenKind::Eof, (0, 0).into()));

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

    pub fn get_or_intern(&mut self, value: impl AsRef<str>) -> lasso::Spur {
        // compiler is not async/threaded (at least for now), so we're unwrapping lock
        let mut interner = self.interner.lock().unwrap();

        interner.get_or_intern(value)
    }

    pub fn report(&mut self, err: ParserError) {
        self.errors.push(err);
        self.panic_mode = true;
    }

    pub fn current(&self) -> &Token {
        &self.current
    }

    pub fn current_clone(&self) -> Token {
        self.current.clone()
    }

    pub fn at(&self, kind: TokenKind) -> bool {
        self.current.kind == kind
    }

    pub fn is_eof(&self) -> bool {
        self.current.kind == TokenKind::Eof
    }

    pub fn eof_token(&self) -> Token {
        let span = (self.current.span.offset() + self.current.span.len(), 0).into();
        Token::new(TokenKind::Eof, span)
    }

    pub fn advance(&mut self) -> Option<Token> {
        let next = self
            .peeked
            .take()
            .or_else(|| self.tokens.next())
            .or_else(|| {
                self.current = self.eof_token();
                None
            })?;

        let prev = self.current.clone();

        self.current = next;

        if self.current.kind == TokenKind::Unknown {
            self.report(ParserError::UnknownToken {
                src: self.named_src(),
                span: self.current.span,
            });
        }

        Some(prev)
    }

    pub fn advance_not_eof(&mut self) -> Option<Token> {
        self.advance().or_else(|| {
            self.report(ParserError::UnexpectedEof {
                expected: "expression".into(),

                src: self.named_src(),
                span: (self.src.len() - 1, 0).into(),
            });

            None
        })
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

    pub fn expect(&mut self, kind: TokenKind, display: &str) -> Option<Token> {
        if self.at(kind) {
            Some(self.advance().unwrap())
        } else {
            if self.current.kind != TokenKind::Eof {
                self.report(ParserError::ExpectedToken {
                    expected: SmolStr::from(display),

                    src: self.named_src(),
                    span: self.current.span,
                });
            } else {
                self.report(ParserError::UnexpectedEof {
                    expected: SmolStr::from(display),

                    src: self.named_src(),
                    span: (self.src.len() - 1, 0).into(),
                });
            }

            None
        }
    }

    pub fn sync(&mut self) {
        self.panic_mode = false;

        loop {
            match self.current().kind {
                TokenKind::Eof => break,

                TokenKind::Semicolon => {
                    let _ = self.advance();
                    break;
                }

                TokenKind::Keyword(ref kw) if is_sync_keyword(kw) => break,

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
