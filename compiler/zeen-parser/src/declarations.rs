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
            TokenKind::Keyword(CompilerKeyword::Struct) => self.parse_struct(start_span, is_pub),
            TokenKind::Keyword(CompilerKeyword::Enum) => self.parse_enum(start_span, is_pub),
            TokenKind::Keyword(CompilerKeyword::Use) => self.parse_use(),
            TokenKind::Keyword(CompilerKeyword::Interface) => {
                self.parse_interface(start_span, is_pub)
            }
            TokenKind::Keyword(CompilerKeyword::Implement) => self.parse_implement(start_span),

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
        let mut body = None;

        if !(self.p.at(TokenKind::OpenBrace)
            || self.p.at(TokenKind::CloseBrace)
            || self.p.at(TokenKind::Semicolon)
            || self.p.at(TokenKind::Eof))
        {
            let mut type_parser = TypeParser::new(self.p);
            return_type = Some(type_parser.parse()?);
        }

        if self.p.at(TokenKind::OpenBrace) {
            let mut stmt_parser = StmtParser::new(self.p);
            body = Some(stmt_parser.parse()?);
        }

        let _ = self.p.eat(TokenKind::Semicolon);

        let latest_span = if let Some(body) = body {
            body.span
        } else if let Some(return_type) = return_type {
            return_type.span
        } else {
            close_params.span
        };

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

    fn parse_struct(
        &mut self,
        start_span: miette::SourceSpan,
        is_pub: IsPub,
    ) -> Option<&'ctx Declaration<'ctx>> {
        #[derive(PartialEq)]
        enum Mode {
            Any,
            Methods,
            Reported,
        };

        let struct_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Struct), "struct")?;

        let name_token = self.p.expect(TokenKind::Ident, "identifier")?;
        let name_span = name_token.span;
        let name_slice =
            self.p.src[name_span.offset()..name_span.offset() + name_span.len()].to_owned();

        let name = (self.p.get_or_intern(name_slice), name_span);

        let mut type_parser = TypeParser::new(self.p);

        // will give Option::None if not at bracket token
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

                let decl = self.parse_fn(start_span, is_pub, IsExtern(false))?;
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

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::StructDecl {
                name,
                is_pub: is_pub.0,
                generics,
                fields,
                methods,
            },
            span,
        });

        Some(decl)
    }

    fn parse_enum(
        &mut self,
        start_span: miette::SourceSpan,
        is_pub: IsPub,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let enum_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Enum), "enum")?;

        let name_token = self.p.expect(TokenKind::Ident, "identifier")?;
        let name_span = name_token.span;
        let name_slice =
            self.p.src[name_span.offset()..name_span.offset() + name_span.len()].to_owned();

        let name = (self.p.get_or_intern(name_slice), name_span);

        let _ = self.p.expect(TokenKind::OpenBrace, "{")?;

        let mut variants_buffer: SmallVec<[declarations::EnumVariant; 8]> = SmallVec::new();

        while !(self.p.at(TokenKind::CloseBrace) || self.p.at(TokenKind::Eof)) {
            let name_token = self.p.expect(TokenKind::Ident, "identifier")?;
            let name_span = name_token.span;
            let name_slice =
                self.p.src[name_span.offset()..name_span.offset() + name_span.len()].to_owned();

            let name = self.p.get_or_intern(name_slice);

            if !(self.p.at(TokenKind::CloseBrace) || self.p.at(TokenKind::Eof)) {
                let _ = self.p.expect(TokenKind::Comma, ",")?;
            }

            variants_buffer.push(declarations::EnumVariant {
                name,
                span: name_span,
            });
        }

        let close_brace = self.p.expect(TokenKind::CloseBrace, "{")?;
        let _ = self.p.eat(TokenKind::Semicolon);

        let variants = self.p.arena.alloc_slice_copy(&variants_buffer);

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::EnumDecl {
                name,
                variants,
                is_pub: is_pub.0,
            },
            span: close_brace.merge_span(start_span),
        });

        Some(decl)
    }

    fn parse_use(&mut self) -> Option<&'ctx Declaration<'ctx>> {
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

        let mut end = current.span;

        let _ = self.p.expect(TokenKind::Semicolon, ";")?;

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::Use { module },
            span: use_kw.merge_span(end),
        });

        Some(decl)
    }

    fn parse_link(&mut self, start_span: miette::SourceSpan) -> Option<&'ctx Declaration<'ctx>> {
        let link_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Link), "link")?;

        let string_token = self.p.expect(
            TokenKind::Literal {
                kind: zeen_lexer::token::LiteralKind::Str { terminated: true },
            },
            "str",
        )?;
        let token_span = string_token.span;
        let string_slice =
            self.p.src[token_span.offset()..token_span.offset() + token_span.len()].to_owned();

        let path = self.p.get_or_intern(string_slice);

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::ExternLink { path },
            span: link_kw.merge_span(string_token.span),
        });

        let _ = self.p.eat(TokenKind::Semicolon);

        Some(decl)
    }

    fn parse_include(&mut self, start_span: miette::SourceSpan) -> Option<&'ctx Declaration<'ctx>> {
        let include_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Include), "include")?;

        let string_token = self.p.expect(
            TokenKind::Literal {
                kind: zeen_lexer::token::LiteralKind::Str { terminated: true },
            },
            "str",
        )?;
        let token_span = string_token.span;
        let string_slice =
            self.p.src[token_span.offset()..token_span.offset() + token_span.len()].to_owned();

        let path = self.p.get_or_intern(string_slice);

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::ExternInclude { path },
            span: include_kw.merge_span(string_token.span),
        });

        let _ = self.p.eat(TokenKind::Semicolon);

        Some(decl)
    }

    fn parse_interface(
        &mut self,
        start_span: miette::SourceSpan,
        is_pub: IsPub,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let interface_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Interface), "interface")?;

        let name_token = self.p.expect(TokenKind::Ident, "identifier")?;
        let name_span = name_token.span;
        let name_slice =
            self.p.src[name_span.offset()..name_span.offset() + name_span.len()].to_owned();

        let name = (self.p.get_or_intern(name_slice), name_span);

        // will give Option::None if not at bracket token
        let mut type_parser = TypeParser::new(self.p);
        let generics = type_parser.parse_generics_declarations();

        let _ = self.p.expect(TokenKind::OpenBrace, "{")?;

        let mut methods: SmallVec<[&'ctx Declaration<'ctx>; 8]> = SmallVec::new();

        while !(self.p.at(TokenKind::CloseBrace) || self.p.at(TokenKind::Eof)) {
            let span_start = self.p.current().span;
            let is_pub = IsPub(false);
            let is_extern = IsExtern(false);

            let decl = self.parse_fn(span_start, is_pub, is_extern)?;

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
            span,
        });

        Some(decl)
    }

    fn parse_implement(
        &mut self,
        start_span: miette::SourceSpan,
    ) -> Option<&'ctx Declaration<'ctx>> {
        let implement_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Implement), "implement")?;

        let mut expr_parser = ExprParser::new(self.p).non_struct_braces();
        let interface = expr_parser.parse()?;

        let _ = self.p.expect(TokenKind::Colon, ":")?;

        // dont blame me, just thanks borrow checker for this repeat
        let mut expr_parser = ExprParser::new(self.p).non_struct_braces();
        let object = expr_parser.parse()?;

        let _ = self.p.expect(TokenKind::OpenBrace, "{")?;

        let mut methods: SmallVec<[&'ctx Declaration<'ctx>; 8]> = SmallVec::new();

        while !(self.p.at(TokenKind::CloseBrace) || self.p.at(TokenKind::Eof)) {
            let span_start = self.p.current().span;
            let is_pub = IsPub(false);
            let is_extern = IsExtern(false);

            let decl = self.parse_fn(span_start, is_pub, is_extern)?;

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
                object,
                methods,
            },
            span,
        });

        Some(decl)
    }

    fn parse_let(&mut self, start_span: miette::SourceSpan) -> Option<&'ctx Declaration<'ctx>> {
        let let_kw = self
            .p
            .expect(TokenKind::Keyword(CompilerKeyword::Let), "let")?;

        let name_token = self.p.expect(TokenKind::Ident, "identifier")?;
        let name_span = name_token.span;
        let name_slice =
            self.p.src[name_span.offset()..name_span.offset() + name_span.len()].to_owned();

        let name = (self.p.get_or_intern(name_slice), name_span);

        let _ = self.p.expect(TokenKind::Colon, ":")?;

        let mut type_parser = TypeParser::new(self.p);
        let ty = type_parser.parse()?;

        let _ = self.p.expect(TokenKind::Semicolon, ";")?;

        let decl = self.p.arena.alloc(Declaration {
            kind: DeclarationKind::ExternVar { name, ty },
            span: ty.merge_span(start_span),
        });

        Some(decl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    use zeen_ast::{Expression, ExpressionKind, Statement, StatementKind, TypeExpr, TypeKind};

    macro_rules! make_parser {
        ($src:expr, $tokens:ident, $bump:ident, $rodeo:ident, $parser:ident) => {
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
        };
    }

    #[test]
    fn fn_decl_basic() {
        const SRC: &str = "fn foo();";

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
                    is_extern: false,
                },
                ..
            }])
        );
    }

    #[test]
    fn fn_decl_public() {
        const SRC: &str = "public fn foo();";

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
        const SRC: &str = "public extern fn foo();";

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
        const SRC: &str = "fn foo() i32;";

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
                    body: None,
                    is_pub: false,
                    is_extern: false,
                },
                ..
            }])
        );
    }

    #[test]
    fn fn_decl_with_unnamed_params() {
        const SRC: &str = "fn foo(i32, u32) i32;";

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
                    body: None,
                    is_pub: false,
                    is_extern: false,
                },
                ..
            }])
        );
    }

    #[test]
    fn fn_decl_with_named_params() {
        const SRC: &str = "fn foo(a: i32, b: u32) i32;";

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
                    body: None,
                    is_pub: false,
                    is_extern: false,
                },
                ..
            }])
        );
    }

    #[test]
    fn fn_decl_with_generics() {
        const SRC: &str = "fn foo[T: Add + Display, R: Debug + Copy](a: i32, b: u32) i32;";

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
                    body: None,
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
        const SRC: &str = "public struct Foo {}";

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
            public b: u32
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
            public b: u32,

            public fn new(a: i32) Self {}

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
                },
                ..
            }])
        );
    }

    #[test]
    fn enum_decl_pub() {
        const SRC: &str = "public enum Foo {}";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::EnumDecl {
                    name: _,
                    variants: [],
                    is_pub: true,
                },
                ..
            }])
        );
    }

    #[test]
    fn enum_decl_with_variants() {
        const SRC: &str = "public enum Foo { A, B, C }";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::EnumDecl {
                    name: _,
                    variants: [
                        zeen_ast::declarations::EnumVariant { name: _, span: _ },
                        zeen_ast::declarations::EnumVariant { name: _, span: _ },
                        zeen_ast::declarations::EnumVariant { name: _, span: _ },
                    ],
                    is_pub: true,
                },
                ..
            }])
        );
    }

    #[test]
    fn import_decl_single() {
        const SRC: &str = "use std;";

        make_parser!(SRC, tokens, bump, rodeo, parser);

        assert_matches!(
            parser.parse_program(),
            Ok([Declaration {
                kind: DeclarationKind::Use { module: _ },
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
                kind: DeclarationKind::Use { module: _ },
                ..
            }])
        );
    }

    // WARNING: Deprecated test

    // #[test]
    // fn import_decl_with_alias() {
    //     const SRC: &str = "import std.io.Stdout : default_output;";
    //
    //     make_parser!(SRC, tokens, bump, rodeo, parser);
    //
    //     assert_matches!(
    //         parser.parse_program(),
    //         Ok([Declaration {
    //             kind: DeclarationKind::Use { module: _ },
    //             ..
    //         }])
    //     );
    // }

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
                    }
                },
                ..
            }])
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
        const SRC: &str = "public interface Empty {}";

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
        const SRC: &str = "interface Empty[T, R] {}";

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
}
