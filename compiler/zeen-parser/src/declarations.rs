use crate::{
    Parser, error::ParserError, expressions::ExprParser, statements::StmtParser,
    type_parser::TypeParser,
};

use smallvec::SmallVec;

use zeen_ast::{Declaration, DeclarationKind, Expression, Statement, TypeExpr, declarations};
use zeen_lexer::{Token, TokenKind, token::CompilerKeyword};

pub struct DeclParser<'ctx, 'pr> {
    p: &'pr mut Parser<'ctx>,
}

struct IsPub(bool);
struct IsExtern(bool);

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

        let is_pub = IsPub(self.p.eat(TokenKind::Keyword(CompilerKeyword::Public)));

        match self.p.current().kind {
            TokenKind::Keyword(CompilerKeyword::Extern) => {
                let _ = self.p.advance_not_eof()?;

                match self.p.current().kind {
                    TokenKind::Keyword(CompilerKeyword::Fn) => {
                        self.parse_fn(start_span, is_pub, IsExtern(true))
                    }
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

            TokenKind::Keyword(CompilerKeyword::Fn) => {
                self.parse_fn(start_span, is_pub, IsExtern(false))
            }
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
    fn parse_fn(
        &mut self,
        start_span: miette::SourceSpan,
        is_pub: IsPub,
        is_extern: IsExtern,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let fn_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Fn), "fn")?;

        let name_token = self.p.expect(TokenKind::Ident, "identifier")?;
        let name_span = name_token.span;
        let name_slice =
            self.p.src[name_span.offset()..name_span.offset() + name_span.len()].to_owned();

        let name = (self.p.get_or_intern(name_slice), name_span);

        let mut type_parser = TypeParser::new(self.p);

        // will give Option::None if not at bracket token
        let generics = type_parser.parse_generics_declarations();

        let _ = self.p.expect(TokenKind::OpenParen, "(")?;

        let mut params_buffer: SmallVec<[declarations::FnParam; 4]> = SmallVec::new();

        while !(self.p.at(TokenKind::CloseParen) || self.p.at(TokenKind::Eof)) {
            let mut name: Option<lasso::Spur> = None;
            let mut span = self.p.current().span;

            if self.p.at(TokenKind::Ident) {
                let name_token = self.p.advance_not_eof()?;
                let name_span = name_token.span;
                let name_slice = 
                    self.p.src[name_span.offset() .. name_span.offset() + name_span.len()].to_owned();

                name = Some(self.p.get_or_intern(name_slice));
                span = name_span;

                let _ = self.p.expect(TokenKind::Colon, ":")?;
            }

            let mut type_parser = TypeParser::new(self.p);
            let ty = type_parser.parse()?;

            span = ty.merge_span(span);

            let _ = self.p.eat(TokenKind::Comma);

            params_buffer.push(declarations::FnParam {
                name,
                ty,
                span
            });
        }

        let close_params = self.p.expect(TokenKind::CloseParen, ")")?;

        let params = self.p.arena.alloc_slice_copy(&params_buffer);

        let mut return_type = None;
        let mut body = None;

        if !(self.p.at(TokenKind::OpenBrace) || self.p.at(TokenKind::Semicolon) || self.p.at(TokenKind::Eof)) {
            let mut type_parser = TypeParser::new(self.p);
            return_type = Some(type_parser.parse()?);
        }

        if self.p.at(TokenKind::OpenBrace) {
            let mut stmt_parser = StmtParser::new(self.p);
            body = Some(stmt_parser.parse()?);
        }

        let _ = self.p.eat(TokenKind::Semicolon);

        let latest_span = if let Some(body) = body { body.span }
            else if let Some(return_type) = return_type { return_type.span }
            else { close_params.span };

        let span = fn_kw.merge_span(latest_span);

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::FnDecl {
                name,
                generics,
                params,
                return_type,
                body,
                is_pub: is_pub.0,
                is_extern: is_extern.0,
            },
            span,
        });

        Some(decl)
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
