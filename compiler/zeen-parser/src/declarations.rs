use crate::{
    Parser, error::ParserError, expressions::ExprParser, statements::StmtParser,
    type_parser::TypeParser,
};

use smallvec::SmallVec;

use zeen_ast::{Declaration, DeclarationKind, Expression, Statement, TypeExpr};
use zeen_lexer::{Token, TokenKind, token::CompilerKeyword};

pub struct DeclParser<'ctx, 'pr> {
    p: &'pr mut Parser<'ctx>,
}

/// ==@ Declarations Parser @==
impl<'ctx, 'pr> DeclParser<'ctx, 'pr> {
    pub fn new(parser: &'pr mut Parser<'ctx>) -> Self {
        Self { p: parser }
    }

    pub fn errors(&self) -> &[ParserError] {
        &self.p.errors
    }
}

impl<'ctx, 'pr> DeclParser<'ctx, 'pr> {
    pub fn parse(&mut self) -> Option<&'ctx Declaration<'ctx>> {
        if self.p.panic_mode {
            self.p.sync()
        }

        if self.p.at(TokenKind::Eof) {
            return None;
        }

        let start_span = self.p.current().span;

        let is_pub = self.p.eat(TokenKind::Keyword(CompilerKeyword::Public));

        match self.p.current().kind {
            TokenKind::Keyword(CompilerKeyword::Extern) => {
                let _ = self.p.advance_not_eof()?;

                match self.p.current().kind {
                    TokenKind::Keyword(CompilerKeyword::Fn) => self.parse_fn(start_span),
                    TokenKind::Keyword(CompilerKeyword::Link) => self.parse_link(start_span),
                    TokenKind::Keyword(CompilerKeyword::Include) => self.parse_include(start_span),
                    TokenKind::Keyword(CompilerKeyword::Let) => self.parse_let(start_span),

                    _ => {
                        self.p.report(ParserError::SyntaxError {
                            label: "not supported for `extern` declaration".into(),
                            help: None,
                            src: self.p.named_src(),
                            span: self.p.current.span,
                        });

                        None
                    }
                }
            }

            TokenKind::Keyword(CompilerKeyword::Fn) => self.parse_fn(start_span),
            TokenKind::Keyword(CompilerKeyword::Struct) => self.parse_struct(start_span),
            TokenKind::Keyword(CompilerKeyword::Enum) => self.parse_enum(start_span),
            TokenKind::Keyword(CompilerKeyword::Import) => self.parse_import(),

            _ => {
                self.p.report(ParserError::UnknownDeclaration {
                    token_kind: format!("{:?}", self.p.current.kind).into(),
                    src: self.p.named_src(),
                    span: self.p.current.span,
                });

                None
            }
        }
    }
}

impl<'ctx, 'pr> DeclParser<'ctx, 'pr> {
    fn parse_fn(&mut self, start_span: miette::SourceSpan) -> Option<&'ctx Declaration<'ctx>> {
        todo!()
    }

    fn parse_struct(&mut self, start_span: miette::SourceSpan) -> Option<&'ctx Declaration<'ctx>> {
        todo!()
    }

    fn parse_enum(&mut self, start_span: miette::SourceSpan) -> Option<&'ctx Declaration<'ctx>> {
        todo!()
    }

    fn parse_import(&mut self) -> Option<&'ctx Declaration<'ctx>> {
        todo!()
    }

    fn parse_link(&mut self, start_span: miette::SourceSpan) -> Option<&'ctx Declaration<'ctx>> {
        todo!()
    }

    fn parse_include(&mut self, start_span: miette::SourceSpan) -> Option<&'ctx Declaration<'ctx>> {
        todo!()
    }

    fn parse_let(&mut self, start_span: miette::SourceSpan) -> Option<&'ctx Declaration<'ctx>> {
        todo!()
    }
}
