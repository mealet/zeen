use crate::{
    Parser, error::ParserError, expressions::ExprParser, statements::StmtParser,
    type_parser::TypeParser,
};

use smallvec::SmallVec;

use lasso::Spur;
use smol_str::SmolStr;
use zeen_ast::{
    Declaration, DeclarationKind, Expression, ExpressionKind, PreprocessorDirective, Statement,
    StatementKind, TypeExpr, TypeKind, declarations,
    declarations::GenericType,
    expressions::Literal,
    types::{BuiltinShape, BuiltinType},
};
use zeen_lexer::{TokenKind, token::CompilerKeyword};

pub struct DeclParser<'tok, 'ctx, 'pr> {
    p: &'pr mut Parser<'tok, 'ctx>,
    name_splices: bool,
}

pub(crate) struct IsPub(pub bool);
pub(crate) struct IsExtern(pub bool);
pub(crate) struct AllowBare(pub bool);

/// ==@ Declarations Parser @==
impl<'tok, 'ctx, 'pr> DeclParser<'tok, 'ctx, 'pr> {
    pub fn new(parser: &'pr mut Parser<'tok, 'ctx>) -> Self {
        Self {
            p: parser,
            name_splices: false,
        }
    }

    pub fn with_name_splices(mut self) -> Self {
        self.name_splices = true;
        self
    }

    pub fn errors(&self) -> &[ParserError] {
        &self.p.errors
    }

    fn parse_decl_name(&mut self) -> Option<(lasso::Spur, miette::SourceSpan)> {
        if self.name_splices && self.p.at(TokenKind::Dollar) {
            return self.parse_name_splice();
        }

        let name_token = self.p.expect(TokenKind::Ident, "identifier")?;
        let name_span = name_token.span;
        let name_slice =
            self.p.src[name_span.offset()..name_span.offset() + name_span.len()].to_owned();

        Some((self.p.get_or_intern(name_slice), name_span))
    }

    fn parse_name_splice(&mut self) -> Option<(lasso::Spur, miette::SourceSpan)> {
        let dollar_span = self.p.current().span;

        let _dollar = self.p.advance_not_eof()?;

        if !self.p.at(TokenKind::OpenParen) {
            self.p.report(ParserError::SyntaxError {
                label: "expected `(` after `$` in a declaration name".into(),
                help: None,
                src: self.p.named_src(),
                span: self.p.current().span,
            });

            return None;
        }

        let open_paren = self.p.advance_not_eof()?;
        let mut end = open_paren.span.offset() + open_paren.span.len();
        let mut depth = 1usize;

        while depth > 0 {
            let token = self.p.advance_not_eof()?;

            match token.kind {
                TokenKind::OpenParen => depth += 1,
                TokenKind::CloseParen => {
                    depth -= 1;
                    end = token.span.offset() + token.span.len();
                }
                TokenKind::Eof => {
                    self.p.report(ParserError::SyntaxError {
                        label: "unterminated `$(` splice in a declaration name".into(),
                        help: None,
                        src: self.p.named_src(),
                        span: dollar_span,
                    });

                    return None;
                }
                _ => {}
            }
        }

        let start = dollar_span.offset();
        let span = (start, end - start).into();
        let text = self.p.src[start..end].to_owned();

        Some((self.p.get_or_intern(text), span))
    }
}

impl<'tok, 'ctx, 'pr> DeclParser<'tok, 'ctx, 'pr> {
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
                        self.parse_fn(start_span, is_pub, IsExtern(true), AllowBare(false))
                    }
                    TokenKind::Keyword(CompilerKeyword::Link) => self.parse_link(start_span),
                    TokenKind::Keyword(CompilerKeyword::Include) => self.parse_include(start_span),
                    TokenKind::Keyword(CompilerKeyword::Let) => {
                        self.parse_let(start_span, is_pub.0)
                    }

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
                self.parse_fn(start_span, is_pub, IsExtern(false), AllowBare(false))
            }
            TokenKind::Keyword(CompilerKeyword::Let) => {
                self.parse_global_var(start_span, is_pub, false)
            }
            TokenKind::Keyword(CompilerKeyword::Const) => {
                self.parse_global_var(start_span, is_pub, true)
            }
            TokenKind::Keyword(CompilerKeyword::Comptime) => {
                let _ = self.p.advance_not_eof()?;

                match self.p.current().kind {
                    TokenKind::Keyword(CompilerKeyword::Fn) => {
                        self.parse_comptime_fn(start_span, is_pub)
                    }
                    TokenKind::Keyword(CompilerKeyword::Struct) => {
                        self.parse_struct(start_span, is_pub, true)
                    }
                    TokenKind::OpenBrace => self.parse_comptime_block(start_span),
                    _ => self.parse_comptime_const(start_span, is_pub),
                }
            }
            TokenKind::Keyword(CompilerKeyword::Struct) => {
                self.parse_struct(start_span, is_pub, false)
            }
            TokenKind::Keyword(CompilerKeyword::Enum) => self.parse_enum(start_span, is_pub),
            TokenKind::Keyword(CompilerKeyword::Alias) => self.parse_alias(start_span, is_pub),
            TokenKind::Keyword(CompilerKeyword::Use) => self.parse_use(is_pub),
            TokenKind::Keyword(CompilerKeyword::Interface) => {
                self.parse_interface(start_span, is_pub)
            }
            TokenKind::Keyword(CompilerKeyword::Implement) => self.parse_implement(start_span),

            TokenKind::PreprocessorIdent
            | TokenKind::PreprocessorDebug
            | TokenKind::PreprocessorRelease => self.parse_conditional_block(is_pub),

            _ => {
                self.p.report(ParserError::UnknownDeclaration {
                    token_kind: format!("{:?}", self.p.current.kind).into(),
                    src: self.p.named_src(),
                    span: self.p.current.span,
                });

                self.p.decl_sync();

                None
            }
        }
    }
}

impl<'tok, 'ctx, 'pr> DeclParser<'tok, 'ctx, 'pr> {
    pub(crate) fn parse_conditional_block(
        &mut self,
        _is_pub: IsPub,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let start_token = self.p.current_clone();
        let start_span = start_token.span;

        let name_slice =
            self.p.src[start_span.offset() + 1..start_span.offset() + start_span.len()].to_owned();

        let directive = match self.p.parse_preprocessor_directive() {
            Some(d) => d,
            None => {
                self.p.report(ParserError::SyntaxError {
                    label: format!("unknown preprocessor directive `@{name_slice}`").into(),
                    help: Some(
                        "expected one of os, arch, env, target, family, debug, release".into(),
                    ),
                    src: self.p.named_src(),
                    span: start_span,
                });
                return None;
            }
        };

        let _ = self.p.advance_not_eof()?;

        let values = self.p.parse_directive_values(directive)?;
        let body = self.parse_directive_body()?;
        let else_block = self.parse_directive_else()?;

        let block = self.p.arena.alloc(zeen_ast::ConditionalBlock {
            directive,
            values,
            body,
            bare_else: false,
            else_block,
        });

        let span = start_token.merge_span(self.p.current().span);

        Some(self.p.arena.alloc(Declaration {
            kind: DeclarationKind::ConditionalBlock(block),
            source: (span, self.p.named_src()).into(),
        }))
    }

    fn parse_directive_body(&mut self) -> Option<&'ctx [&'ctx Declaration<'ctx>]> {
        let _ = self.p.expect(TokenKind::OpenBrace, "{")?;

        let mut buffer: SmallVec<[&'ctx Declaration<'ctx>; 8]> = SmallVec::new();
        while !self.p.at(TokenKind::CloseBrace) && !self.p.at(TokenKind::Eof) {
            let mut decl_parser = DeclParser::new(self.p);
            if let Some(decl) = decl_parser.parse() {
                buffer.push(decl);
            }
            if self.p.panic_mode {
                break;
            }
        }

        let _ = self.p.expect(TokenKind::CloseBrace, "}")?;
        Some(self.p.arena.alloc_slice_copy(&buffer))
    }

    fn parse_directive_else(&mut self) -> Option<Option<&'ctx Declaration<'ctx>>> {
        if !self.p.at(TokenKind::Keyword(CompilerKeyword::Else)) {
            return Some(None);
        }

        let _ = self.p.advance_not_eof()?;

        if self.p.at(TokenKind::OpenBrace) {
            let body = self.parse_directive_body()?;
            let block = self.p.arena.alloc(zeen_ast::ConditionalBlock {
                directive: PreprocessorDirective::Debug,
                values: self.p.arena.alloc_slice_copy(&[]),
                body,
                bare_else: true,
                else_block: None,
            });
            let decl = self.p.arena.alloc(Declaration {
                kind: DeclarationKind::ConditionalBlock(block),
                source: (self.p.current().span, self.p.named_src()).into(),
            });
            return Some(Some(decl));
        }

        self.parse_conditional_block(IsPub(false)).map(Some)
    }
}

impl<'tok, 'ctx, 'pr> DeclParser<'tok, 'ctx, 'pr> {
    pub(crate) fn parse_fn(
        &mut self,
        _start_span: miette::SourceSpan,
        is_pub: IsPub,
        is_extern: IsExtern,
        allow_bare: AllowBare,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let fn_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Fn), "fn")?;

        let name = self.parse_decl_name()?;

        let mut type_parser = TypeParser::new(self.p);

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
                    self.p.src[name_span.offset()..name_span.offset() + name_span.len()].to_owned();

                name = Some(self.p.get_or_intern(name_slice));
                span = name_span;

                let _ = self.p.expect(TokenKind::Colon, ":")?;
            }

            let mut type_parser = TypeParser::new(self.p);
            let ty = type_parser.parse()?;

            span = ty.merge_span(span);

            let _ = self.p.eat(TokenKind::Comma);

            params_buffer.push(declarations::FnParam { name, ty, span });
        }

        let close_params = self.p.expect(TokenKind::CloseParen, ")")?;

        let params = self.p.arena.alloc_slice_copy(&params_buffer);

        let mut return_type = None;
        if !(self.p.at(TokenKind::OpenBrace)
            || self.p.at(TokenKind::CloseBrace)
            || self.p.at(TokenKind::Semicolon)
            || self.p.at(TokenKind::Eof))
        {
            let mut type_parser = TypeParser::new(self.p).no_typeof();
            return_type = Some(type_parser.parse()?);
        }

        let body: Option<&'ctx Statement<'ctx>> = if self.p.at(TokenKind::OpenBrace) {
            let open = self.p.expect(TokenKind::OpenBrace, "{")?;
            let mut stmts_buffer: SmallVec<[&'ctx Statement<'ctx>; 8]> = SmallVec::new();

            while !(self.p.at(TokenKind::CloseBrace) || self.p.at(TokenKind::Eof)) {
                let mut stmt_parser =
                    crate::statements::StmtParser::new(self.p).with_optional_semicolon(true);
                if let Some(stmt) = stmt_parser.parse() {
                    stmts_buffer.push(stmt);
                }
            }

            let close = self.p.expect(TokenKind::CloseBrace, "}")?;

            let stmts: &'ctx [&'ctx Statement<'ctx>] = self.p.arena.alloc_slice_copy(&stmts_buffer);
            let trailing: Option<&'ctx Expression<'ctx>> = stmts.last().and_then(|stmt| {
                if let StatementKind::TrailingExpr(expr) = stmt.kind {
                    Some(expr)
                } else {
                    None
                }
            });

            let (stmts, trailing) = if let Some(tr) = trailing {
                let len = stmts.len();
                let trimmed: &'ctx [&'ctx Statement<'ctx>] =
                    self.p.arena.alloc_slice_copy(&stmts[..len - 1]);
                (trimmed, Some(tr))
            } else {
                (stmts, None)
            };

            let block_expr = self.p.arena.alloc(Expression {
                kind: ExpressionKind::Block { stmts, trailing },
                span: open.merge_span(close.span),
            });
            let body_stmt = Statement {
                kind: StatementKind::Expr(block_expr),
                span: block_expr.span,
            };
            Some(self.p.arena.alloc(body_stmt))
        } else {
            None
        };

        let _ = self.p.eat(TokenKind::Semicolon);

        let latest_span = if let Some(body) = body {
            body.span
        } else if let Some(return_type) = return_type {
            return_type.span
        } else {
            close_params.span
        };

        let span = fn_kw.merge_span(latest_span);

        if body.is_none() && !is_extern.0 && !allow_bare.0 {
            self.p.report(ParserError::SyntaxError {
                label: "function declaration without a body".into(),
                help: Some("only `extern fn` may be declared without a body".into()),
                src: self.p.named_src(),
                span,
            });

            // The declaration parses fine, so recovery must not skip the next one.
            self.p.panic_mode = false;
        }

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
            source: (span, self.p.named_src()).into(),
        });

        Some(decl)
    }

    fn parse_alias(
        &mut self,
        _start_span: miette::SourceSpan,
        is_pub: IsPub,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let alias_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Alias), "alias")?;

        let name = self.parse_decl_name()?;

        let generics = {
            let mut type_parser = TypeParser::new(self.p);

            // will give Option::None if not at bracket token
            type_parser.parse_generics_declarations()
        };

        let _ = self.p.expect(TokenKind::Eq, "=")?;

        let mut type_parser = TypeParser::new(self.p);
        let ty = type_parser.parse()?;

        let _ = self.p.eat(TokenKind::Semicolon);

        let span = alias_kw.merge_span(ty.span);

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::Alias(declarations::AliasDecl {
                name,
                is_pub: is_pub.0,
                generics,
                ty,
            }),
            source: (span, self.p.named_src()).into(),
        });

        Some(decl)
    }

    fn parse_comptime_fn(
        &mut self,
        start_span: miette::SourceSpan,
        is_pub: IsPub,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let fn_decl = self.parse_fn(start_span, is_pub, IsExtern(false), AllowBare(false))?;

        let (name, generics, params, return_type, body, is_pub) = match &fn_decl.kind {
            DeclarationKind::FnDecl {
                name,
                generics,
                params,
                return_type,
                body,
                is_pub,
                ..
            } => (*name, *generics, *params, *return_type, *body, *is_pub),
            _ => return Some(fn_decl),
        };

        if let Some(generics) = generics
            && let Some(first) = generics.first()
        {
            self.p.report(ParserError::SyntaxError {
                label: "comptime functions cannot have generic parameters".into(),
                help: Some("generic parameters inside comptime are planned for v2".into()),
                src: self.p.named_src(),
                span: first.name.1,
            });
        }

        let fn_span = fn_decl.merge_span(start_span);
        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::ComptimeFn {
                name,
                params,
                return_type,
                body,
                is_pub,
            },
            source: (fn_span, fn_decl.source.src()).into(),
        });

        Some(decl)
    }

    fn parse_comptime_const(
        &mut self,
        _start_span: miette::SourceSpan,
        is_pub: IsPub,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let name = self.parse_decl_name()?;

        let _ = self.p.expect(TokenKind::Eq, "= initial value")?;

        let mut expr_parser = ExprParser::new(self.p);
        let value = expr_parser.parse()?;

        let _ = self.p.expect(TokenKind::Semicolon, ";")?;

        let span = value.merge_span(_start_span);

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::ComptimeConst {
                name,
                value,
                is_pub: is_pub.0,
            },
            source: (span, self.p.named_src()).into(),
        });

        Some(decl)
    }

    fn parse_comptime_block(
        &mut self,
        _start_span: miette::SourceSpan,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let open = self.p.expect(TokenKind::OpenBrace, "{")?;

        let mut buffer: SmallVec<[&'ctx Statement<'ctx>; 8]> = SmallVec::new();
        while !(self.p.at(TokenKind::CloseBrace) || self.p.at(TokenKind::Eof)) {
            let mut stmt_parser = StmtParser::new(self.p).with_optional_semicolon(true);
            if let Some(stmt) = stmt_parser.parse() {
                buffer.push(stmt);
            }
            if self.p.panic_mode {
                break;
            }
        }

        let close = self.p.expect(TokenKind::CloseBrace, "}")?;

        let stmts = self.p.arena.alloc_slice_copy(&buffer);

        let block = self.p.arena.alloc(declarations::ComptimeBlock { stmts });

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::ComptimeBlock(block),
            source: (open.merge_span(close.span), self.p.named_src()).into(),
        });

        Some(decl)
    }

    fn parse_struct(
        &mut self,
        start_span: miette::SourceSpan,
        is_pub: IsPub,
        is_comptime: bool,
    ) -> Option<&'ctx Declaration<'ctx>> {
        #[derive(PartialEq)]
        enum Mode {
            Any,
            Methods,
            Reported,
        }

        let _struct_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Struct), "struct")?;

        let name = self.parse_decl_name()?;

        let mut type_parser = TypeParser::new(self.p);

        let generics = type_parser.parse_generics_declarations();

        let _ = self.p.expect(TokenKind::OpenBrace, "{")?;

        let mut fields_buffer: SmallVec<[declarations::StructField; 8]> = SmallVec::new();
        let mut methods_buffer: SmallVec<[&'ctx Declaration<'ctx>; 8]> = SmallVec::new();

        let mut mode = Mode::Any;

        while !(self.p.at(TokenKind::CloseBrace) || self.p.at(TokenKind::Eof)) {
            let start_span = self.p.current().span;
            let is_pub = IsPub(self.p.eat(TokenKind::Keyword(CompilerKeyword::Public)));

            if self.p.at(TokenKind::Keyword(CompilerKeyword::Fn)) {
                if mode == Mode::Any {
                    mode = Mode::Methods;
                }

                let decl = self.parse_fn(start_span, is_pub, IsExtern(false), AllowBare(false))?;
                methods_buffer.push(decl);

                let _ = self.p.eat(TokenKind::Comma);

                continue;
            }

            let name_token = self.p.expect(TokenKind::Ident, "identifier")?;
            let name_span = name_token.span;
            let name_slice =
                self.p.src[name_span.offset()..name_span.offset() + name_span.len()].to_owned();

            let name = self.p.get_or_intern(name_slice);

            if self.p.at(TokenKind::OpenParen) || self.p.at(TokenKind::OpenBracket) {
                self.p.report(ParserError::SyntaxError {
                    label: "function definition without `fn` keyword".into(),
                    help: Some("consider using syntax: [public] fn IDENT(..) ..".into()),
                    src: self.p.named_src(),
                    span: name_span,
                });

                return None;
            }

            let _ = self.p.expect(TokenKind::Colon, ":")?;

            let mut type_parser = TypeParser::new(self.p);
            let ty = type_parser.parse()?;

            let struct_field = declarations::StructField {
                name,
                ty,
                is_pub: is_pub.0,
            };

            fields_buffer.push(struct_field);

            let _ = self.p.eat(TokenKind::Comma);

            if mode == Mode::Methods {
                mode = Mode::Reported;

                self.p.report(ParserError::SyntaxError {
                    label: "fields are not allowed after methods".into(),
                    help: Some("consider defining necessary fields before methods".into()),
                    src: self.p.named_src(),
                    span: name_span,
                });
            }
        }

        let close_brace = self.p.expect(TokenKind::CloseBrace, "{")?;
        let _ = self.p.eat(TokenKind::Semicolon);

        let fields = self.p.arena.alloc_slice_copy(&fields_buffer);
        let methods = self.p.arena.alloc_slice_copy(&methods_buffer);

        let span = close_brace.merge_span(start_span);

        let kind = if is_comptime {
            if let Some(generics) = generics
                && let Some(first) = generics.first()
            {
                self.p.report(ParserError::SyntaxError {
                    label: "comptime structs cannot have generic parameters".into(),
                    help: Some("use `zeen_type` fields instead".into()),
                    src: self.p.named_src(),
                    span: first.name.1,
                });
            }

            DeclarationKind::ComptimeStruct {
                name,
                is_pub: is_pub.0,
                fields,
                methods,
            }
        } else {
            DeclarationKind::StructDecl {
                name,
                is_pub: is_pub.0,
                generics,
                fields,
                methods,
            }
        };

        let decl = self.p.arena.alloc(Declaration {
            kind,
            source: (span, self.p.named_src()).into(),
        });

        Some(decl)
    }

    fn parse_field_list(&mut self) -> Option<&'ctx [declarations::StructField<'ctx>]> {
        let _ = self.p.expect(TokenKind::OpenBrace, "{")?;

        let mut fields_buffer: SmallVec<[declarations::StructField; 8]> = SmallVec::new();

        while !(self.p.at(TokenKind::CloseBrace) || self.p.at(TokenKind::Eof)) {
            let _ = self.p.eat(TokenKind::Keyword(CompilerKeyword::Public));

            if self.p.at(TokenKind::Keyword(CompilerKeyword::Fn)) {
                self.p.report(ParserError::SyntaxError {
                    label: "methods are not allowed in anonymous struct payloads".into(),
                    help: Some("consider defining methods on the enum itself".into()),
                    src: self.p.named_src(),
                    span: self.p.current().span,
                });

                return None;
            }

            let name_token = self.p.expect(TokenKind::Ident, "identifier")?;
            let name_span = name_token.span;
            let name_slice =
                self.p.src[name_span.offset()..name_span.offset() + name_span.len()].to_owned();

            let name = self.p.get_or_intern(name_slice);

            if self.p.at(TokenKind::OpenParen) || self.p.at(TokenKind::OpenBracket) {
                self.p.report(ParserError::SyntaxError {
                    label: "function definition without `fn` keyword".into(),
                    help: Some("consider using syntax: [public] fn IDENT(..) ..".into()),
                    src: self.p.named_src(),
                    span: name_span,
                });

                return None;
            }

            let _ = self.p.expect(TokenKind::Colon, ":")?;

            let mut type_parser = TypeParser::new(self.p);
            let ty = type_parser.parse()?;

            fields_buffer.push(declarations::StructField {
                name,
                ty,
                is_pub: true,
            });

            let _ = self.p.eat(TokenKind::Comma);
        }

        let _ = self.p.expect(TokenKind::CloseBrace, "{")?;

        Some(self.p.arena.alloc_slice_copy(&fields_buffer))
    }

    fn parse_enum(
        &mut self,
        start_span: miette::SourceSpan,
        is_pub: IsPub,
    ) -> Option<&'ctx Declaration<'ctx>> {
        #[derive(PartialEq)]
        enum Mode {
            Any,
            Methods,
            Reported,
        }

        let _enum_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Enum), "enum")?;

        let name = self.parse_decl_name()?;

        let mut type_parser = TypeParser::new(self.p);
        let generics = type_parser.parse_generics_declarations();

        let _ = self.p.expect(TokenKind::OpenBrace, "{")?;

        let mut variants_buffer: SmallVec<[declarations::EnumVariant; 8]> = SmallVec::new();
        let mut methods_buffer: SmallVec<[&'ctx Declaration<'ctx>; 8]> = SmallVec::new();

        let mut mode = Mode::Any;

        while !(self.p.at(TokenKind::CloseBrace) || self.p.at(TokenKind::Eof)) {
            let start_span = self.p.current().span;
            let is_pub = IsPub(self.p.eat(TokenKind::Keyword(CompilerKeyword::Public)));

            if self.p.at(TokenKind::Keyword(CompilerKeyword::Fn)) {
                if mode == Mode::Any {
                    mode = Mode::Methods;
                }

                let decl = self.parse_fn(start_span, is_pub, IsExtern(false), AllowBare(false))?;
                methods_buffer.push(decl);

                let _ = self.p.eat(TokenKind::Comma);

                continue;
            }

            let name_token = self.p.expect(TokenKind::Ident, "identifier")?;
            let name_span = name_token.span;
            let name_slice =
                self.p.src[name_span.offset()..name_span.offset() + name_span.len()].to_owned();

            let name = self.p.get_or_intern(name_slice);

            if mode == Mode::Methods {
                mode = Mode::Reported;

                self.p.report(ParserError::SyntaxError {
                    label: "variants are not allowed after methods".into(),
                    help: Some("consider defining necessary variants before methods".into()),
                    src: self.p.named_src(),
                    span: name_span,
                });
            }

            let mut payload = None;

            if self.p.eat(TokenKind::Colon) {
                if self.p.at(TokenKind::OpenBrace) {
                    let fields = self.parse_field_list()?;
                    payload = Some(declarations::EnumVariantPayload::Anonymous(fields));
                } else {
                    let mut type_parser = TypeParser::new(self.p);
                    let ty = type_parser.parse()?;
                    payload = Some(declarations::EnumVariantPayload::Single(ty));
                }
            }

            let _ = self.p.eat(TokenKind::Comma);

            variants_buffer.push(declarations::EnumVariant {
                name,
                span: name_span,
                payload,
            });
        }

        let close_brace = self.p.expect(TokenKind::CloseBrace, "{")?;
        let _ = self.p.eat(TokenKind::Semicolon);

        let variants = self.p.arena.alloc_slice_copy(&variants_buffer);
        let methods = self.p.arena.alloc_slice_copy(&methods_buffer);

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::EnumDecl {
                name,
                is_pub: is_pub.0,
                generics,
                variants,
                methods,
            },
            source: (close_brace.merge_span(start_span), self.p.named_src()).into(),
        });

        Some(decl)
    }

    fn parse_use(&mut self, is_pub: IsPub) -> Option<&'ctx Declaration<'ctx>> {
        let use_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Use), "use")?;

        let mut module_name = String::new();

        let ident_token = self.p.expect(TokenKind::Ident, "identifier")?;
        let ident_span = ident_token.span;
        let ident_slice = &self.p.src[ident_span.offset()..ident_span.offset() + ident_span.len()];

        module_name.push_str(ident_slice);

        let mut current = ident_token;

        if self.p.at(TokenKind::Ident) || self.p.at(TokenKind::Dot) {
            current = self.p.current_clone();
        }

        while self.p.at(TokenKind::Ident) || self.p.at(TokenKind::Dot) {
            let current_span = current.span;
            let current_slice =
                &self.p.src[current_span.offset()..current_span.offset() + current_span.len()];

            module_name.push_str(current_slice);
            let _ = self.p.advance_not_eof()?;

            if self.p.at(TokenKind::Ident) || self.p.at(TokenKind::Dot) {
                current = self.p.current_clone();
            }
        }

        if current.kind == TokenKind::Dot {
            self.p.report(ParserError::SyntaxError {
                label: "import sequence ends with dot (`.`)".into(),
                help: Some("consider removing last dot from module import".into()),
                src: self.p.named_src(),
                span: current.span,
            });

            return None;
        }

        let module_id = self.p.get_or_intern(module_name);
        let module = (module_id, ident_token.merge_span(current.span));

        let end = current.span;

        let _ = self.p.expect(TokenKind::Semicolon, ";")?;

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::Use {
                module,
                is_pub: is_pub.0,
            },
            source: (use_kw.merge_span(end), self.p.named_src()).into(),
        });

        Some(decl)
    }

    fn parse_link(&mut self, start_span: miette::SourceSpan) -> Option<&'ctx Declaration<'ctx>> {
        let _link_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Link), "link")?;

        let string_token = self.p.expect(
            TokenKind::Literal {
                kind: zeen_lexer::token::LiteralKind::Str { terminated: true },
            },
            "str",
        )?;
        let token_span = string_token.span;
        let string_slice = self.p.src
            [token_span.offset() + 1..token_span.offset() + token_span.len() - 1]
            .to_owned();

        let path = self.p.get_or_intern(string_slice);

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::ExternLink { path },
            source: (string_token.merge_span(start_span), self.p.named_src()).into(),
        });

        let _ = self.p.eat(TokenKind::Semicolon);

        Some(decl)
    }

    fn parse_include(&mut self, start_span: miette::SourceSpan) -> Option<&'ctx Declaration<'ctx>> {
        let _include_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Include), "include")?;

        let string_token = self.p.expect(
            TokenKind::Literal {
                kind: zeen_lexer::token::LiteralKind::Str { terminated: true },
            },
            "str",
        )?;
        let token_span = string_token.span;
        let string_slice = self.p.src
            [token_span.offset() + 1..token_span.offset() + token_span.len() - 1]
            .to_owned();

        let path = self.p.get_or_intern(string_slice);

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::ExternInclude { path },
            source: (string_token.merge_span(start_span), self.p.named_src()).into(),
        });

        let _ = self.p.eat(TokenKind::Semicolon);

        Some(decl)
    }

    fn parse_interface(
        &mut self,
        start_span: miette::SourceSpan,
        is_pub: IsPub,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let _interface_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Interface), "interface")?;

        // will give Option::None if not at bracket token
        let mut type_parser = TypeParser::new(self.p);
        let generics = type_parser.parse_generics_declarations();

        let name = self.parse_decl_name()?;

        let _ = self.p.expect(TokenKind::OpenBrace, "{")?;

        let mut methods: SmallVec<[&'ctx Declaration<'ctx>; 8]> = SmallVec::new();

        while !(self.p.at(TokenKind::CloseBrace) || self.p.at(TokenKind::Eof)) {
            let span_start = self.p.current().span;
            let is_pub = IsPub(self.p.eat(TokenKind::Keyword(CompilerKeyword::Public)));
            let is_extern = IsExtern(false);

            let decl = self.parse_fn(span_start, is_pub, is_extern, AllowBare(true))?;

            debug_assert!(matches!(decl.kind, DeclarationKind::FnDecl { .. }));

            methods.push(decl);
        }

        let close_brace = self.p.expect(TokenKind::CloseBrace, "}")?;
        let span = close_brace.merge_span(start_span);

        let _ = self.p.eat(TokenKind::Comma);
        let methods = self.p.arena.alloc_slice_copy(&methods);

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::InterfaceDecl {
                name,
                is_pub: is_pub.0,

                generics,
                methods,
            },
            source: (span, self.p.named_src()).into(),
        });

        Some(decl)
    }

    fn parse_implement(
        &mut self,
        start_span: miette::SourceSpan,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let _implement_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Implement), "implement")?;

        // will give Option::None if not at bracket token
        let mut type_parser = TypeParser::new(self.p);
        let generics = type_parser.parse_generics_declarations();

        let builtin_target = if self.p.at(TokenKind::Keyword(CompilerKeyword::Builtin)) {
            Some(self.parse_builtin_implement_target(generics)?)
        } else {
            None
        };

        let (interface, object_name, object_span) = if let Some((_, name, span)) = builtin_target {
            (None, name, span)
        } else {
            let first_token = self.p.expect(TokenKind::Ident, "identifier")?;
            let first_span = first_token.span;
            let first_slice =
                self.p.src[first_span.offset()..first_span.offset() + first_span.len()].to_owned();

            let first = (self.p.get_or_intern(first_slice), first_span);

            if self.p.eat(TokenKind::Colon) {
                if self.p.at(TokenKind::Keyword(CompilerKeyword::Builtin)) {
                    self.p.report(ParserError::UnsupportedAction {
                        label: SmolStr::from(
                            "interface implementations for builtin types are not supported",
                        ),
                        src: self.p.named_src(),
                        span: self.p.current().span,
                    });

                    return None;
                }

                let object_token = self.p.expect(TokenKind::Ident, "identifier")?;
                let object_span = object_token.span;
                let object_slice = self.p.src
                    [object_span.offset()..object_span.offset() + object_span.len()]
                    .to_owned();

                (Some(first), self.p.get_or_intern(object_slice), object_span)
            } else {
                (None, first.0, first_span)
            }
        };

        let mut generic_bindings: Vec<&'ctx TypeExpr<'ctx>> = Vec::new();

        if self.p.eat(TokenKind::OpenBracket) {
            while !(self.p.at(TokenKind::CloseBracket) || self.p.at(TokenKind::Eof)) {
                let mut type_parser = TypeParser::new(self.p);

                let Some(ty) = type_parser.parse() else {
                    break;
                };

                generic_bindings.push(ty);

                let _ = self.p.eat(TokenKind::Comma);
            }

            let _ = self.p.expect(TokenKind::CloseBracket, "]");
        }

        let generics_arena: &_ = self.p.arena.alloc_slice_copy(&generic_bindings);

        let object = (object_name, object_span, generics_arena);

        let _ = self.p.expect(TokenKind::OpenBrace, "{")?;

        let mut methods: SmallVec<[&'ctx Declaration<'ctx>; 8]> = SmallVec::new();

        while !(self.p.at(TokenKind::CloseBrace) || self.p.at(TokenKind::Eof)) {
            let span_start = self.p.current().span;
            let is_pub = IsPub(self.p.eat(TokenKind::Keyword(CompilerKeyword::Public)));
            let is_extern = IsExtern(false);

            let decl = self.parse_fn(span_start, is_pub, is_extern, AllowBare(false))?;

            debug_assert!(matches!(decl.kind, DeclarationKind::FnDecl { .. }));

            methods.push(decl);
        }

        let close_brace = self.p.expect(TokenKind::CloseBrace, "}")?;
        let span = close_brace.merge_span(start_span);

        let _ = self.p.eat(TokenKind::Comma);
        let methods = self.p.arena.alloc_slice_copy(&methods);

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::ImplementDecl {
                interface,
                builtin: builtin_target.map(|(shape, _, _)| shape),
                object,
                generics,
                methods,
            },
            source: (span, self.p.named_src()).into(),
        });

        Some(decl)
    }

    fn parse_builtin_implement_target(
        &mut self,
        generics: Option<&'ctx [GenericType<'ctx>]>,
    ) -> Option<(&'ctx BuiltinShape, Spur, miette::SourceSpan)> {
        let _ = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Builtin), "builtin")?;

        let ty = {
            let mut type_parser = TypeParser::new(self.p);
            type_parser.parse()?
        };

        if self.p.at(TokenKind::OpenBracket) {
            self.p.report(ParserError::UnsupportedAction {
                label: SmolStr::from("builtin implement blocks cannot carry type arguments"),
                src: self.p.named_src(),
                span: self.p.current().span,
            });

            return None;
        }

        let generic_names: Vec<Spur> = generics
            .map(|gs| gs.iter().map(|g| g.name.0).collect())
            .unwrap_or_default();

        let shape = self.builtin_shape_from_type(ty, &generic_names)?;

        let mut used = Vec::new();
        shape.used_params(&mut used);

        for (index, name) in generic_names.iter().enumerate() {
            if used.contains(&(index as u32)) {
                continue;
            }

            let name = self.p.interner.borrow().resolve(name).to_string();

            self.p.report(ParserError::SyntaxError {
                label: SmolStr::from(format!(
                    "generic parameter `{name}` is not used in the builtin implement target"
                )),
                help: None,
                src: self.p.named_src(),
                span: ty.span,
            });

            return None;
        }

        let slice = self.p.src[ty.span.offset()..ty.span.offset() + ty.span.len()].to_owned();
        let shape = self.p.arena.alloc(shape);

        Some((shape, self.p.get_or_intern(slice), ty.span))
    }

    fn builtin_shape_from_type(
        &mut self,
        ty: &'ctx TypeExpr<'ctx>,
        generic_names: &[Spur],
    ) -> Option<BuiltinShape> {
        match ty.kind {
            TypeKind::Builtin(builtin) => {
                if matches!(builtin, BuiltinType::void | BuiltinType::never) {
                    self.p.report(ParserError::UnsupportedAction {
                        label: SmolStr::from(
                            "`void` and `never` cannot have builtin implement blocks",
                        ),
                        src: self.p.named_src(),
                        span: ty.span,
                    });

                    return None;
                }

                Some(BuiltinShape::Scalar(builtin))
            }

            TypeKind::Array { element, len } => {
                let (element, is_const) = self.builtin_shape_element(element, generic_names)?;

                let Some(len_expr) = len else {
                    return Some(BuiltinShape::Slice {
                        element: Box::new(element),
                        is_const,
                    });
                };

                let len = match len_expr.kind {
                    ExpressionKind::Literal(Literal::Int(value)) => value,
                    _ => {
                        self.p.report(ParserError::UnsupportedAction {
                            label: SmolStr::from(
                                "builtin array implement targets require an integer literal length",
                            ),
                            src: self.p.named_src(),
                            span: len_expr.span,
                        });

                        return None;
                    }
                };

                if len <= 0 {
                    self.p.report(ParserError::UnsupportedAction {
                        label: SmolStr::from(
                            "builtin array implement targets require a positive length",
                        ),
                        src: self.p.named_src(),
                        span: len_expr.span,
                    });

                    return None;
                }

                Some(BuiltinShape::Array {
                    element: Box::new(element),
                    len: len as u64,
                })
            }

            TypeKind::ManyPointer(element) => {
                let (element, is_const) = self.builtin_shape_element(element, generic_names)?;

                Some(BuiltinShape::ManyPointer {
                    element: Box::new(element),
                    is_const,
                })
            }

            TypeKind::Named { name, generic_args } => {
                if generic_args.is_some() {
                    self.p.report(ParserError::UnsupportedAction {
                        label: SmolStr::from(
                            "builtin implement targets cannot carry type arguments",
                        ),
                        src: self.p.named_src(),
                        span: ty.span,
                    });

                    return None;
                }

                match generic_names.iter().position(|generic| *generic == name) {
                    Some(index) => Some(BuiltinShape::Param(index as u32)),
                    None => {
                        self.p.report(ParserError::SyntaxError {
                            label: SmolStr::from(
                                "builtin implement targets may only name generic parameters of the block",
                            ),
                            help: None,
                            src: self.p.named_src(),
                            span: ty.span,
                        });

                        None
                    }
                }
            }

            TypeKind::SinglePointer(_) => {
                self.p.report(ParserError::UnsupportedAction {
                    label: SmolStr::from("pointer types cannot be builtin implement targets"),
                    src: self.p.named_src(),
                    span: ty.span,
                });

                None
            }

            _ => {
                self.p.report(ParserError::UnsupportedAction {
                    label: SmolStr::from("unsupported builtin implement target"),
                    src: self.p.named_src(),
                    span: ty.span,
                });

                None
            }
        }
    }

    fn builtin_shape_element(
        &mut self,
        ty: &'ctx TypeExpr<'ctx>,
        generic_names: &[Spur],
    ) -> Option<(BuiltinShape, bool)> {
        if let TypeKind::Const(inner) = ty.kind {
            Some((self.builtin_shape_from_type(inner, generic_names)?, true))
        } else {
            Some((self.builtin_shape_from_type(ty, generic_names)?, false))
        }
    }

    fn parse_global_var(
        &mut self,
        start_span: miette::SourceSpan,
        is_pub: IsPub,
        is_const: bool,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let kw = if is_const {
            self.p
                .expect(TokenKind::Keyword(CompilerKeyword::Const), "const")
        } else {
            self.p
                .expect(TokenKind::Keyword(CompilerKeyword::Let), "let")
        }?;

        let name = self.parse_decl_name()?;

        let _ = self.p.expect(
            TokenKind::Colon,
            "type annotation (global variables require an explicit type)",
        )?;

        let mut type_parser = TypeParser::new(self.p);
        let ty = type_parser.parse()?;

        let _ = self.p.expect(
            TokenKind::Eq,
            "initializer (global variables require an initial value)",
        )?;

        let mut expr_parser = ExprParser::new(self.p);
        let value = expr_parser.parse()?;

        let _ = self.p.expect(TokenKind::Semicolon, ";")?;

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::GlobalVar {
                name,
                ty,
                value,
                is_const,
                is_pub: is_pub.0,
            },
            source: (
                value.merge_span(kw.merge_span(start_span)),
                self.p.named_src(),
            )
                .into(),
        });

        Some(decl)
    }

    fn parse_let(
        &mut self,
        start_span: miette::SourceSpan,
        is_pub: bool,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let _let_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Let), "let")?;

        let name = self.parse_decl_name()?;

        let _ = self.p.expect(TokenKind::Colon, ":")?;

        let mut type_parser = TypeParser::new(self.p);
        let ty = type_parser.parse()?;

        let _ = self.p.expect(TokenKind::Semicolon, ";")?;

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::ExternVar { name, ty, is_pub },
            source: (ty.merge_span(start_span), self.p.named_src()).into(),
        });

        Some(decl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    use std::{cell::RefCell, rc::Rc};

    use zeen_ast::{Expression, ExpressionKind, TypeExpr, TypeKind};

    macro_rules! make_parser {
        ($src:expr, $tokens:ident, $bump:ident, $rodeo:ident, $parser:ident) => {
            let src_arc = std::sync::Arc::new($src.to_string());
            let $rodeo = Rc::new(RefCell::new(lasso::Rodeo::default()));
            let $bump = bumpalo::Bump::new();
            let mut $tokens = zeen_lexer::tokenize($src);
            let mut $parser = Parser::new(
                Rc::new("tests.zn".to_string()),
                src_arc,
                &mut $tokens,
                &$bump,
                Rc::clone(&$rodeo),
            );
        };
    }

    #[test]
    fn fn_decl_basic() {
        const SRC: &str = "fn foo() {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::FnDecl {
                    name: _,
                    generics: None,
                    params: [],
                    return_type: None,
                    body: Some(_),
                    is_pub: false,
                    is_extern: false,
                },
                ..
            }])
        );
    }

    #[test]
    fn fn_body_span_ends_at_closing_brace() {
        const SRC: &str = "fn foo() {}\n\nfn bar() {}";
        const FIRST: &str = "fn foo() {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        let program = parser.parse_program().unwrap();
        let DeclarationKind::FnDecl {
            body: Some(body), ..
        } = &program[0].kind
        else {
            panic!("expected fn declaration");
        };

        assert_eq!(body.span.offset() + body.span.len(), FIRST.len());
        assert_eq!(
            program[0].source.span.offset() + program[0].source.span.len(),
            FIRST.len()
        );
    }

    #[test]
    fn fn_decl_without_body_rejected() {
        const SRC: &str = "fn foo();";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        let errors = parser.parse_program().unwrap_err();
        assert_matches!(errors[0], ParserError::SyntaxError { .. });
    }

    #[test]
    fn fn_decl_public() {
        const SRC: &str = "pub fn foo() {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::FnDecl {
                    name: _,
                    generics: None,
                    params: [],
                    return_type: None,
                    body: Some(_),
                    is_pub: true,
                    is_extern: false,
                },
                ..
            }])
        );
    }

    #[test]
    fn fn_decl_extern() {
        const SRC: &str = "extern fn foo();";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::FnDecl {
                    name: _,
                    generics: None,
                    params: [],
                    return_type: None,
                    body: None,
                    is_pub: false,
                    is_extern: true,
                },
                ..
            }])
        );
    }

    #[test]
    fn fn_decl_public_extern() {
        const SRC: &str = "pub extern fn foo();";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::FnDecl {
                    name: _,
                    generics: None,
                    params: [],
                    return_type: None,
                    body: None,
                    is_pub: true,
                    is_extern: true,
                },
                ..
            }])
        );
    }

    #[test]
    fn fn_decl_with_return_type() {
        const SRC: &str = "fn foo() i32 {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::FnDecl {
                    name: _,
                    generics: None,
                    params: [],
                    return_type: Some(TypeExpr {
                        kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    }),
                    body: Some(_),
                    is_pub: false,
                    is_extern: false,
                },
                ..
            }])
        );
    }

    #[test]
    fn fn_decl_with_unnamed_params() {
        const SRC: &str = "fn foo(i32, u32) i32 {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::FnDecl {
                    name: _,
                    generics: None,
                    params: [
                        declarations::FnParam {
                            name: None,
                            ty: TypeExpr {
                                kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                                ..
                            },
                            span: _
                        },
                        declarations::FnParam {
                            name: None,
                            ty: TypeExpr {
                                kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::u32),
                                ..
                            },
                            span: _
                        },
                    ],
                    return_type: Some(TypeExpr {
                        kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    }),
                    body: Some(_),
                    is_pub: false,
                    is_extern: false,
                },
                ..
            }])
        );
    }

    #[test]
    fn fn_decl_with_named_params() {
        const SRC: &str = "fn foo(a: i32, b: u32) i32 {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::FnDecl {
                    name: _,
                    generics: None,
                    params: [
                        declarations::FnParam {
                            name: Some(_),
                            ty: TypeExpr {
                                kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                                ..
                            },
                            span: _
                        },
                        declarations::FnParam {
                            name: Some(_),
                            ty: TypeExpr {
                                kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::u32),
                                ..
                            },
                            span: _
                        },
                    ],
                    return_type: Some(TypeExpr {
                        kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    }),
                    body: Some(_),
                    is_pub: false,
                    is_extern: false,
                },
                ..
            }])
        );
    }

    #[test]
    fn fn_decl_with_generics() {
        const SRC: &str = "fn foo[T: Add + Display, R: Debug + Copy](a: i32, b: u32) i32 {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::FnDecl {
                    name: _,
                    generics: Some([
                        declarations::GenericType {
                            name: _,
                            interfaces: Some(_)
                        },
                        declarations::GenericType {
                            name: _,
                            interfaces: Some(_)
                        },
                    ]),
                    params: [
                        declarations::FnParam {
                            name: Some(_),
                            ty: TypeExpr {
                                kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                                ..
                            },
                            span: _
                        },
                        declarations::FnParam {
                            name: Some(_),
                            ty: TypeExpr {
                                kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::u32),
                                ..
                            },
                            span: _
                        },
                    ],
                    return_type: Some(TypeExpr {
                        kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    }),
                    body: Some(_),
                    is_pub: false,
                    is_extern: false,
                },
                ..
            }])
        );
    }

    #[test]
    fn struct_decl_empty() {
        const SRC: &str = "struct Foo {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::StructDecl {
                    name: _,
                    is_pub: false,

                    generics: None,
                    fields: [],
                    methods: []
                },
                ..
            }])
        );
    }

    #[test]
    fn struct_decl_pub() {
        const SRC: &str = "pub struct Foo {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::StructDecl {
                    name: _,
                    is_pub: true,

                    generics: None,
                    fields: [],
                    methods: []
                },
                ..
            }])
        );
    }

    #[test]
    fn struct_decl_with_fields() {
        const SRC: &str = "struct Foo {
            a: i32,
            pub b: u32
        }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::StructDecl {
                    name: _,
                    is_pub: false,

                    generics: None,
                    fields: [
                        zeen_ast::declarations::StructField {
                            name: _,
                            ty: _,
                            is_pub: false,
                        },
                        zeen_ast::declarations::StructField {
                            name: _,
                            ty: _,
                            is_pub: true,
                        },
                    ],
                    methods: []
                },
                ..
            }])
        );
    }

    #[test]
    fn struct_decl_with_methods() {
        const SRC: &str = "struct Foo {
            a: i32,
            pub b: u32,

            pub fn new(a: i32) Self {}

            fn asd(self) u32 {}
            fn dsa(const self) i32 {}
        }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::StructDecl {
                    name: _,
                    is_pub: false,

                    generics: None,
                    fields: [
                        zeen_ast::declarations::StructField {
                            name: _,
                            ty: _,
                            is_pub: false,
                        },
                        zeen_ast::declarations::StructField {
                            name: _,
                            ty: _,
                            is_pub: true,
                        },
                    ],
                    methods: [
                        Declaration {
                            kind: DeclarationKind::FnDecl { .. },
                            ..
                        },
                        Declaration {
                            kind: DeclarationKind::FnDecl { .. },
                            ..
                        },
                        Declaration {
                            kind: DeclarationKind::FnDecl { .. },
                            ..
                        },
                    ]
                },
                ..
            }])
        );
    }

    #[test]
    fn enum_decl_empty() {
        const SRC: &str = "enum Foo {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::EnumDecl {
                    name: _,
                    variants: [],
                    is_pub: false,
                    ..
                },
                ..
            }])
        );
    }

    #[test]
    fn enum_decl_pub() {
        const SRC: &str = "pub enum Foo {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::EnumDecl {
                    name: _,
                    variants: [],
                    is_pub: true,
                    ..
                },
                ..
            }])
        );
    }

    #[test]
    fn enum_decl_with_variants() {
        const SRC: &str = "pub enum Foo { A, B, C }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::EnumDecl {
                    name: _,
                    variants: [
                        zeen_ast::declarations::EnumVariant {
                            name: _,
                            span: _,
                            payload: None,
                        },
                        zeen_ast::declarations::EnumVariant {
                            name: _,
                            span: _,
                            payload: None,
                        },
                        zeen_ast::declarations::EnumVariant {
                            name: _,
                            span: _,
                            payload: None,
                        },
                    ],
                    is_pub: true,
                    ..
                },
                ..
            }])
        );
    }

    #[test]
    fn enum_decl_single_payload() {
        const SRC: &str = "enum Foo { a, b: i32 }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::EnumDecl {
                    variants: [
                        zeen_ast::declarations::EnumVariant { payload: None, .. },
                        zeen_ast::declarations::EnumVariant {
                            payload: Some(zeen_ast::declarations::EnumVariantPayload::Single(_)),
                            ..
                        },
                    ],
                    ..
                },
                ..
            }])
        );
    }

    #[test]
    fn enum_decl_anonymous_payload() {
        const SRC: &str = "enum Foo { c: { inner: i32, hello: u32 } }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::EnumDecl {
                    variants: [zeen_ast::declarations::EnumVariant {
                        payload: Some(zeen_ast::declarations::EnumVariantPayload::Anonymous([
                            _,
                            _
                        ])),
                        ..
                    }],
                    ..
                },
                ..
            }])
        );
    }

    #[test]
    fn enum_decl_generics() {
        const SRC: &str = "enum Opt[T: Copy] { none, some: T }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::EnumDecl {
                    generics: Some([_]),
                    variants: [_, _],
                    ..
                },
                ..
            }])
        );
    }

    #[test]
    fn enum_decl_with_methods() {
        const SRC: &str = "enum Foo { a, fn tag(self) u8 { return 0; } }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::EnumDecl {
                    variants: [_],
                    methods: [_],
                    ..
                },
                ..
            }])
        );
    }

    #[test]
    fn enum_decl_rejects_variant_after_method() {
        const SRC: &str = "enum Foo { a, fn tag(self) u8 { return 0; }, b }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert!(parser.parse_program().is_err());
    }

    #[test]
    fn import_decl_single() {
        const SRC: &str = "use std;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::Use { .. },
                ..
            }])
        );
    }

    #[test]
    fn import_decl_nested() {
        const SRC: &str = "use std.io.stdout;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::Use { .. },
                ..
            }])
        );
    }

    #[test]
    fn pub_use_marks_reexport() {
        const SRC: &str = "pub use std.io;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::Use { is_pub: true, .. },
                ..
            }])
        );
    }

    #[test]
    fn plain_use_is_private() {
        const SRC: &str = "use std.io;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::Use { is_pub: false, .. },
                ..
            }])
        );
    }

    #[test]
    fn link_decl() {
        const SRC: &str = "extern link \"test.c\";";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::ExternLink { path: _ },
                ..
            }])
        );
    }

    #[test]
    fn include_decl() {
        const SRC: &str = "extern include \"test.h\";";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::ExternInclude { path: _ },
                ..
            }])
        );
    }

    #[test]
    fn let_decl() {
        const SRC: &str = "extern let abcd: i32;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::ExternVar {
                    name: _,
                    ty: TypeExpr {
                        kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    },
                    is_pub: _,
                },
                ..
            }])
        );
    }

    #[test]
    fn pub_let_decl() {
        const SRC: &str = "pub extern let abcd: i32;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::ExternVar {
                    name: _,
                    ty: TypeExpr {
                        kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    },
                    is_pub: true,
                },
                ..
            }])
        );
    }

    #[test]
    fn global_let_decl() {
        const SRC: &str = "let global: i32 = 0;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::GlobalVar {
                    name: _,
                    ty: TypeExpr {
                        kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    },
                    value: Expression {
                        kind: ExpressionKind::Literal(zeen_ast::expressions::Literal::Int(0)),
                        ..
                    },
                    is_const: false,
                    is_pub: false,
                },
                ..
            }])
        );
    }

    #[test]
    fn global_let_decl_pub() {
        const SRC: &str = "pub let global: i32 = 0;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::GlobalVar {
                    name: _,
                    ty: TypeExpr {
                        kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    },
                    value: Expression {
                        kind: ExpressionKind::Literal(zeen_ast::expressions::Literal::Int(0)),
                        ..
                    },
                    is_const: false,
                    is_pub: true,
                },
                ..
            }])
        );
    }

    #[test]
    fn global_const_decl() {
        const SRC: &str = "const global: i32 = 0;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::GlobalVar {
                    name: _,
                    ty: TypeExpr {
                        kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    },
                    value: Expression {
                        kind: ExpressionKind::Literal(zeen_ast::expressions::Literal::Int(0)),
                        ..
                    },
                    is_const: true,
                    is_pub: false,
                },
                ..
            }])
        );
    }

    #[test]
    fn global_const_decl_pub() {
        const SRC: &str = "pub const global: i32 = 0;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::GlobalVar {
                    name: _,
                    ty: TypeExpr {
                        kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    },
                    value: Expression {
                        kind: ExpressionKind::Literal(zeen_ast::expressions::Literal::Int(0)),
                        ..
                    },
                    is_const: true,
                    is_pub: true,
                },
                ..
            }])
        );
    }

    #[test]
    fn global_var_with_expr_value() {
        const SRC: &str = "let global: i32 = 1 + 2 * 3;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::GlobalVar {
                    name: _,
                    ty: TypeExpr {
                        kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::i32),
                        ..
                    },
                    value: Expression {
                        kind: ExpressionKind::Binary { .. },
                        ..
                    },
                    is_const: false,
                    is_pub: false,
                },
                ..
            }])
        );
    }

    #[test]
    fn global_vars_mixed_with_fn() {
        const SRC: &str = "let a: i32 = 0; pub const b: i32 = 1; fn main() {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([
                Declaration {
                    kind: DeclarationKind::GlobalVar {
                        name: _,
                        is_const: false,
                        is_pub: false,
                        ..
                    },
                    ..
                },
                Declaration {
                    kind: DeclarationKind::GlobalVar {
                        name: _,
                        is_const: true,
                        is_pub: true,
                        ..
                    },
                    ..
                },
                Declaration {
                    kind: DeclarationKind::FnDecl { .. },
                    ..
                },
            ])
        );
    }

    #[test]
    fn interface_decl() {
        const SRC: &str = "interface Empty {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::InterfaceDecl {
                    name: _,
                    is_pub: false,

                    generics: None,
                    methods: [],
                },
                ..
            }])
        );
    }

    #[test]
    fn interface_decl_public() {
        const SRC: &str = "pub interface Empty {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::InterfaceDecl {
                    name: _,
                    is_pub: true,

                    generics: None,
                    methods: [],
                },
                ..
            }])
        );
    }

    #[test]
    fn interface_decl_with_generics() {
        const SRC: &str = "interface[T, R] Empty {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::InterfaceDecl {
                    name: _,
                    is_pub: false,

                    generics: Some([
                        zeen_ast::declarations::GenericType {
                            name: _,
                            interfaces: _,
                        },
                        zeen_ast::declarations::GenericType {
                            name: _,
                            interfaces: _,
                        },
                    ]),
                    methods: [],
                },
                ..
            }])
        );
    }

    #[test]
    fn interface_decl_with_method() {
        const SRC: &str = "interface Default {
            fn default() Self;
            fn abcd_lol() **i32;
        }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::InterfaceDecl {
                    name: _,
                    is_pub: false,

                    generics: None,
                    methods: [
                        Declaration {
                            kind: DeclarationKind::FnDecl { .. },
                            ..
                        },
                        Declaration {
                            kind: DeclarationKind::FnDecl { .. },
                            ..
                        }
                    ],
                },
                ..
            }])
        );
    }

    #[test]
    fn interface_methods_support_pub() {
        const SRC: &str = "interface Display {
            pub fn display(*const self) []const char;
            fn hidden(*const self) []const char;
        }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::InterfaceDecl {
                    name: _,
                    is_pub: false,

                    generics: None,
                    methods: [
                        Declaration {
                            kind: DeclarationKind::FnDecl {
                                name: _,
                                is_pub: true,
                                ..
                            },
                            ..
                        },
                        Declaration {
                            kind: DeclarationKind::FnDecl {
                                name: _,
                                is_pub: false,
                                ..
                            },
                            ..
                        }
                    ],
                },
                ..
            }])
        );
    }

    #[test]
    fn implement_methods_support_pub() {
        const SRC: &str = "implement Display : Foo {
            pub fn display(*const self) void {}
        }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::ImplementDecl {
                    interface: _,
                    builtin: _,
                    object: _,
                    generics: None,
                    methods: [Declaration {
                        kind: DeclarationKind::FnDecl {
                            name: _,
                            is_pub: true,
                            ..
                        },
                        ..
                    }],
                },
                ..
            }])
        );
    }

    #[test]
    fn implement_builtin_block_parses() {
        const SRC: &str = "implement builtin i32 {
            fn is_even(*const self) bool {
                return (*self) % 2 == 0;
            }
        }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        let program = parser.parse_program().unwrap();

        assert_matches!(
            program[0].kind,
            DeclarationKind::ImplementDecl {
                interface: None,
                builtin: Some(_),
                generics: None,
                methods: [Declaration {
                    kind: DeclarationKind::FnDecl { .. },
                    ..
                }],
                ..
            }
        );

        let DeclarationKind::ImplementDecl { builtin, .. } = &program[0].kind else {
            panic!("expected an implement declaration");
        };

        assert_eq!(builtin.unwrap().key(), "i32");
    }

    #[test]
    fn implement_builtin_slice_target_parses() {
        const SRC: &str = "implement[T] builtin []T {
            fn first(*const self) T {
                return (*self)[0];
            }
        }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        let program = parser.parse_program().unwrap();

        assert_matches!(
            program[0].kind,
            DeclarationKind::ImplementDecl {
                interface: None,
                builtin: Some(_),
                generics: Some([_]),
                methods: [Declaration {
                    kind: DeclarationKind::FnDecl { .. },
                    ..
                }],
                ..
            }
        );

        let DeclarationKind::ImplementDecl { builtin, .. } = &program[0].kind else {
            panic!("expected an implement declaration");
        };

        assert_eq!(builtin.unwrap().key(), "[]#0");
    }

    #[test]
    fn implement_builtin_array_target_parses() {
        const SRC: &str = "implement builtin [4]u8 {
            fn total(*const self) u32 {
                return @as(u32, (*self)[0]);
            }
        }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        let program = parser.parse_program().unwrap();

        assert_matches!(
            program[0].kind,
            DeclarationKind::ImplementDecl {
                interface: None,
                builtin: Some(_),
                generics: None,
                methods: [Declaration {
                    kind: DeclarationKind::FnDecl { .. },
                    ..
                }],
                ..
            }
        );

        let DeclarationKind::ImplementDecl { builtin, .. } = &program[0].kind else {
            panic!("expected an implement declaration");
        };

        assert_eq!(builtin.unwrap().key(), "[4]u8");
    }

    #[test]
    fn implement_builtin_rejects_void() {
        const SRC: &str = "implement builtin void {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert!(parser.parse_program().is_err());
    }

    #[test]
    fn implement_builtin_rejects_unused_generic() {
        const SRC: &str = "implement[T] builtin i32 {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert!(parser.parse_program().is_err());
    }

    #[test]
    fn implement_builtin_rejects_unknown_generic() {
        const SRC: &str = "implement[T] builtin []U {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert!(parser.parse_program().is_err());
    }

    #[test]
    fn implement_builtin_rejects_pointer_target() {
        const SRC: &str = "implement[T] builtin *T {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert!(parser.parse_program().is_err());
    }

    #[test]
    fn implement_builtin_rejects_non_literal_array_len() {
        const SRC: &str = "implement builtin [1 + 1]u8 {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert!(parser.parse_program().is_err());
    }

    #[test]
    fn implement_builtin_rejects_type_args() {
        const SRC: &str = "implement builtin i32[i32] {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert!(parser.parse_program().is_err());
    }

    #[test]
    fn implement_builtin_rejects_interface_form() {
        const SRC: &str = "implement Show : builtin i32 {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert!(parser.parse_program().is_err());
    }

    #[test]
    fn implement_builtin_rejects_non_builtin_name() {
        const SRC: &str = "implement builtin Foo {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert!(parser.parse_program().is_err());
    }

    #[test]
    fn alias_decl_plain() {
        const SRC: &str = "alias Meters = f64;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::Alias(declarations::AliasDecl {
                    name: _,
                    is_pub: false,
                    generics: None,
                    ty: TypeExpr {
                        kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::f64),
                        ..
                    },
                }),
                ..
            }])
        );
    }

    #[test]
    fn alias_decl_public() {
        const SRC: &str = "pub alias Meters = f64;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::Alias(declarations::AliasDecl {
                    name: _,
                    is_pub: true,
                    generics: None,
                    ty: TypeExpr {
                        kind: TypeKind::Builtin(zeen_ast::types::BuiltinType::f64),
                        ..
                    },
                }),
                ..
            }])
        );
    }

    #[test]
    fn alias_decl_with_generics() {
        const SRC: &str = "alias Ptr[T] = *T;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::Alias(declarations::AliasDecl {
                    name: _,
                    is_pub: false,
                    generics: Some([declarations::GenericType {
                        name: _,
                        interfaces: None
                    }]),
                    ty: TypeExpr {
                        kind: TypeKind::SinglePointer(_),
                        ..
                    },
                }),
                ..
            }])
        );
    }

    #[test]
    fn alias_decl_with_bounds() {
        const SRC: &str = "alias Printable[T: Display] = T;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::Alias(declarations::AliasDecl {
                    name: _,
                    is_pub: false,
                    generics: Some([declarations::GenericType {
                        name: _,
                        interfaces: Some(_)
                    }]),
                    ty: TypeExpr { .. },
                }),
                ..
            }])
        );
    }

    #[test]
    fn conditional_block_parses() {
        const SRC: &str = "@os[linux | macos] { fn a() {} }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::ConditionalBlock(zeen_ast::ConditionalBlock {
                    directive: PreprocessorDirective::Os,
                    values: [
                        zeen_ast::DirectiveValue { value: "linux", .. },
                        zeen_ast::DirectiveValue { value: "macos", .. }
                    ],
                    body: [_],
                    bare_else: false,
                    else_block: None,
                }),
                ..
            }])
        );
    }

    #[test]
    fn conditional_block_with_else() {
        const SRC: &str = "@os[windows] { fn a() {} } else { fn b() {} }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::ConditionalBlock(zeen_ast::ConditionalBlock {
                    directive: PreprocessorDirective::Os,
                    values: [_],
                    body: [_],
                    bare_else: false,
                    else_block: Some(Declaration {
                        kind: DeclarationKind::ConditionalBlock(zeen_ast::ConditionalBlock {
                            bare_else: true,
                            body: [_],
                            ..
                        }),
                        ..
                    }),
                }),
                ..
            }])
        );
    }

    #[test]
    fn decl_name_splice_is_kept_raw() {
        const SRC: &str = "@zeen_decl(pub struct $(generate()) { pub value: i32 })";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        let expr = ExprParser::new(&mut parser).parse().unwrap();

        let ExpressionKind::MacroCall { args, .. } = &expr.kind else {
            panic!("expected a macro call");
        };
        let ExpressionKind::Decl(decl) = &args[0].kind else {
            panic!("expected a captured declaration");
        };
        let DeclarationKind::StructDecl { name, .. } = decl.kind else {
            panic!("expected a struct declaration");
        };

        assert_eq!(rodeo.borrow().resolve(&name.0), "$(generate())");
    }

    #[test]
    fn decl_name_splice_outside_zeen_decl_rejected() {
        const SRC: &str = "pub struct $(generate()) { pub value: i32 }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        let errors = parser.parse_program().unwrap_err();
        assert_matches!(errors[0], ParserError::ExpectedToken { .. });
    }
}
