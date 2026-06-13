use crate::{Parser, error::ParserError};

use zeen_ast::{
    declarations::GenericType,
    types::{self, TypeExpr, TypeKind},
};
use zeen_lexer::{Token, TokenKind, token};

use smallvec::SmallVec;

pub struct TypeParser<'tok, 'ctx, 'pr> {
    p: &'pr mut Parser<'tok, 'ctx>,
}

impl<'tok, 'ctx, 'pr> TypeParser<'tok, 'ctx, 'pr> {
    pub fn new(parser: &'pr mut Parser<'tok, 'ctx>) -> Self {
        Self { p: parser }
    }

    pub fn parse(&mut self) -> Option<&'ctx TypeExpr<'ctx>> {
        match self.p.current().kind {
            TokenKind::Type(ref comp_type) => self.parse_builtin(),

            // ptr type: *T
            TokenKind::Star => self.parse_ptr(),

            // array type: [N]T (fixed) or []T (slice)
            TokenKind::OpenBracket => self.parse_array(),

            // fn type: fn(T, ...) T
            TokenKind::Keyword(token::CompilerKeyword::Fn) => self.parse_fn_type(),

            // self / Self
            TokenKind::Keyword(token::CompilerKeyword::SelfLower) => self.parse_self_type(),
            TokenKind::Keyword(token::CompilerKeyword::SelfUpper) => self.parse_self_alias(),

            // const: const T
            TokenKind::Keyword(token::CompilerKeyword::Const) => self.parse_const_type(),

            // named
            TokenKind::Ident => self.parse_named(),

            // va args
            TokenKind::Dot => self.parse_va_args_type(),

            TokenKind::Eof => {
                self.p.report(ParserError::UnexpectedEof {
                    expected: "type".into(),
                    src: self.p.named_src(),
                    span: self.p.current().span,
                });

                None
            }

            _ => {
                self.p.report(ParserError::UnknownType {
                    label: "unknown type here".into(),
                    help: None,

                    src: self.p.named_src(),
                    span: self.p.current().span,
                });

                None
            }
        }
    }
}

impl<'tok, 'ctx, 'pr> TypeParser<'tok, 'ctx, 'pr> {
    pub fn parse_generics_declarations(&mut self) -> Option<&'ctx [GenericType<'ctx>]> {
        if self.p.eat(TokenKind::OpenBracket) {
            let mut generics: SmallVec<[GenericType<'ctx>; 8]> = SmallVec::new();

            loop {
                if self.p.at(TokenKind::CloseBracket) || self.p.at(TokenKind::Eof) {
                    break;
                };

                let name_token = self.p.expect(TokenKind::Ident, "identifier")?;
                let name_slice = self.p.src
                    [name_token.span.offset()..name_token.span.offset() + name_token.span.len()]
                    .to_owned();

                let name = self.p.get_or_intern(name_slice);

                let interfaces: Option<&'ctx [lasso::Spur]> = if self.p.eat(TokenKind::Colon) {
                    let mut interfaces_buffer: SmallVec<[lasso::Spur; 8]> = SmallVec::new();

                    while !matches!(
                        self.p.current().kind,
                        TokenKind::CloseBracket | TokenKind::Eof | TokenKind::Comma
                    ) {
                        let interface_token = self.p.expect(TokenKind::Ident, "identifier")?;
                        let interface_slice = self.p.src[interface_token.span.offset()
                            ..interface_token.span.offset() + interface_token.span.len()]
                            .to_owned();

                        let interface_id = self.p.get_or_intern(interface_slice);
                        interfaces_buffer.push(interface_id);

                        let _ = self.p.eat(TokenKind::Plus);
                    }

                    let interfaces_arena = self.p.arena.alloc_slice_copy(&interfaces_buffer);
                    drop(interfaces_buffer);

                    Some(interfaces_arena)
                } else {
                    None
                };

                let _ = self.p.eat(TokenKind::Comma);

                generics.push(GenericType { name, interfaces });
            }

            self.p.eat(TokenKind::CloseBracket);

            let generics_slice = self.p.arena.alloc_slice_copy(&generics);
            drop(generics);

            return Some(generics_slice);
        }

        None
    }
}

impl<'tok, 'ctx, 'pr> TypeParser<'tok, 'ctx, 'pr> {
    fn parse_builtin(&mut self) -> Option<&'ctx TypeExpr<'ctx>> {
        let token = self.p.current_clone();
        let _ = self.p.advance();

        let TokenKind::Type(typ) = token.kind else {
            unreachable!()
        };

        let builtin_type = types::BuiltinType::try_lexer_type(typ);

        let expr = self.p.arena.alloc(TypeExpr {
            kind: TypeKind::Builtin(builtin_type),
            span: token.span,
        });

        Some(expr)
    }

    fn parse_ptr(&mut self) -> Option<&'ctx TypeExpr<'ctx>> {
        let star = self.p.current_clone();
        let _ = self.p.advance();

        let arena = self.p.arena;

        let mut child = self.parse()?;

        let expr = arena.alloc(TypeExpr {
            kind: TypeKind::Pointer(child),
            span: star.merge_span(child.span),
        });

        Some(expr)
    }

    fn parse_array(&mut self) -> Option<&'ctx TypeExpr<'ctx>> {
        let open = self.p.current_clone();
        let _ = self.p.advance_not_eof()?;

        let arena = self.p.arena;

        let len: Option<&'ctx zeen_ast::Expression> = if self.p.at(TokenKind::CloseBracket) {
            None
        } else {
            let mut expr_parser = crate::expressions::ExprParser::new(self.p);
            Some(expr_parser.parse()?)
        };

        if !self.p.eat(TokenKind::CloseBracket) {
            self.p.report(ParserError::UnknownType {
                label: "array type is not closed".into(),
                help: Some("consider following syntax: `[N]T` / `[]T`".into()),

                src: self.p.named_src(),
                span: open.span,
            });

            return None;
        }

        let element = self.parse()?;

        let expr = arena.alloc(TypeExpr {
            kind: TypeKind::Array { element, len },
            span: open.merge_span(element.span),
        });

        Some(expr)
    }

    fn parse_fn_type(&mut self) -> Option<&'ctx TypeExpr<'ctx>> {
        let kw_fn = self.p.current_clone();
        let _ = self.p.advance_not_eof()?;

        let mut generic_args = self.parse_generics_declarations();

        if !self.p.eat(TokenKind::OpenParen) {
            self.p.report(ParserError::UnknownType {
                label: "invalid fn type syntax".into(),
                help: Some("consider following syntax: `fn(T, ...) T`".into()),

                src: self.p.named_src(),
                span: kw_fn.merge_span(self.p.current.span),
            });

            return None;
        }

        let mut args_types: SmallVec<[&'_ TypeExpr<'ctx>; 16]> = SmallVec::new();

        while !(self.p.at(TokenKind::CloseParen) || self.p.at(TokenKind::Eof)) {
            let type_expr = self.parse()?;
            args_types.push(type_expr);

            let _ = self.p.eat(TokenKind::Comma);
        }

        if !self.p.eat(TokenKind::CloseParen) {
            self.p.report(ParserError::UnknownType {
                label: "signature is not closed".into(),
                help: Some("consider following syntax: `fn(T, ...) T`".into()),

                src: self.p.named_src(),
                span: kw_fn.merge_span(self.p.current.span),
            });

            return None;
        }

        let arena_params = self.p.arena.alloc_slice_clone(&args_types);
        let ret_type = self.parse()?;

        let expr = self.p.arena.alloc(TypeExpr {
            kind: TypeKind::Fn {
                params: arena_params,
                ret: ret_type,
                generic_args,
            },
            span: kw_fn.merge_span(ret_type.span),
        });

        Some(expr)
    }

    fn parse_self_type(&mut self) -> Option<&'ctx TypeExpr<'ctx>> {
        let kw_self = self.p.current();
        let kw_span = kw_self.span;

        let _ = self.p.advance();

        let expr = self.p.arena.alloc(TypeExpr {
            kind: TypeKind::SelfType,
            span: kw_span,
        });

        Some(expr)
    }

    fn parse_self_alias(&mut self) -> Option<&'ctx TypeExpr<'ctx>> {
        let kw_self_upper = self.p.current();
        let kw_span = kw_self_upper.span;

        let _ = self.p.advance();

        let expr = self.p.arena.alloc(TypeExpr {
            kind: TypeKind::SelfAlias,
            span: kw_span,
        });

        Some(expr)
    }

    fn parse_named(&mut self) -> Option<&'ctx TypeExpr<'ctx>> {
        let ident_token = self.p.current_clone();
        let mut span = ident_token.span;

        let ident_slice = self.p.src
            [ident_token.span.offset()..ident_token.span.offset() + ident_token.span.len()]
            .to_owned();

        let name_id = self.p.get_or_intern(ident_slice);
        let _ = self.p.advance();

        let mut generic_args: Option<&'_ [&'_ zeen_ast::TypeExpr<'_>]> = None;

        if self.p.eat(TokenKind::OpenBracket) {
            let mut args_buffer: SmallVec<[&zeen_ast::TypeExpr<'_>; 16]> = SmallVec::new();

            while self.p.current().kind != TokenKind::Eof {
                let current = self.p.current_clone();

                if self.p.eat(TokenKind::CloseBracket) {
                    span = ident_token.merge_span(current.span);
                    break;
                }

                let generic_arg_type = self.parse()?;
                args_buffer.push(generic_arg_type);

                let _ = self.p.eat(TokenKind::Comma);
            }

            let args_slice = self.p.arena.alloc_slice_clone(&args_buffer);
            drop(args_buffer);

            generic_args = Some(args_slice);
        }

        let expr = self.p.arena.alloc(TypeExpr {
            kind: TypeKind::Named {
                name: name_id,
                generic_args,
            },
            span,
        });

        Some(expr)
    }

    fn parse_const_type(&mut self) -> Option<&'ctx TypeExpr<'ctx>> {
        let kw_const = self.p.current_clone();
        let _ = self.p.advance();

        let arena = self.p.arena;

        let mut child = self.parse()?;

        let expr = arena.alloc(TypeExpr {
            kind: TypeKind::Const(child),
            span: kw_const.merge_span(child.span),
        });

        Some(expr)
    }

    fn parse_va_args_type(&mut self) -> Option<&'ctx TypeExpr<'ctx>> {
        let start = self.p.expect(TokenKind::Dot, ".")?;
        let _ = self.p.expect(TokenKind::Dot, ".")?;
        let end = self.p.expect(TokenKind::Dot, ".")?;

        let arena = self.p.arena;

        let expr = arena.alloc(TypeExpr {
            kind: TypeKind::VaArgs,
            span: start.merge_span(end.span),
        });

        Some(expr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! make_type_parser {
        ($src:expr, $tokens:ident, $bump:ident, $rodeo:ident, $parser:ident, $p_name: ident) => {
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
            let mut $p_name = TypeParser::new(&mut $parser);
        };
    }

    #[test]
    fn builtin_integer_types() {
        const SRC: &str = "i8 i16 i32 i64 isize";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        // i8
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::i8),
                span: (0, 2).into()
            }
        );

        // i16
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::i16),
                span: (3, 3).into()
            }
        );

        // i32
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::i32),
                span: (7, 3).into()
            }
        );

        // i64
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::i64),
                span: (11, 3).into()
            }
        );

        // isize
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::isize),
                span: (15, 5).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn builtin_unsigned_integer_types() {
        const SRC: &str = "u8 u16 u32 u64 usize";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        // u8
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::u8),
                span: (0, 2).into()
            }
        );

        // u16
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::u16),
                span: (3, 3).into()
            }
        );

        // u32
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::u32),
                span: (7, 3).into()
            }
        );

        // u64
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::u64),
                span: (11, 3).into()
            }
        );

        // usize
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::usize),
                span: (15, 5).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn builtin_float_types() {
        const SRC: &str = "f32 f64";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        // f32
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::f32),
                span: (0, 3).into()
            }
        );

        // f64
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::f64),
                span: (4, 3).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn builtin_other_types() {
        const SRC: &str = "bool char void";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        // bool
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::bool),
                span: (0, 4).into()
            }
        );

        // char
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::char),
                span: (5, 4).into()
            }
        );

        // void
        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Builtin(types::BuiltinType::void),
                span: (10, 4).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn ptr_type_basic() {
        const SRC: &str = "*i32";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Pointer(&TypeExpr {
                    kind: TypeKind::Builtin(types::BuiltinType::i32),
                    span: (1, 3).into()
                }),
                span: (0, 4).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn ptr_type_nested() {
        const SRC: &str = "***i32";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Pointer(&TypeExpr {
                    kind: TypeKind::Pointer(&TypeExpr {
                        kind: TypeKind::Pointer(&TypeExpr {
                            kind: TypeKind::Builtin(types::BuiltinType::i32),
                            span: (3, 3).into()
                        }),
                        span: (2, 4).into()
                    }),
                    span: (1, 5).into()
                }),
                span: (0, 6).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn array_type() {
        const SRC: &str = "[10]i32";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Array {
                    element: &TypeExpr {
                        kind: TypeKind::Builtin(types::BuiltinType::i32),
                        span: (4, 3).into(),
                    },
                    len: Some(&zeen_ast::Expression {
                        kind: zeen_ast::ExpressionKind::Literal(
                            zeen_ast::expressions::Literal::Int(10)
                        ),
                        span: (1, 2).into(),
                    })
                },
                span: (0, 7).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn slice_type() {
        const SRC: &str = "[]i32";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Array {
                    element: &TypeExpr {
                        kind: TypeKind::Builtin(types::BuiltinType::i32),
                        span: (2, 3).into(),
                    },
                    len: None,
                },
                span: (0, 5).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn fn_type() {
        const SRC: &str = "fn(i32, u32) usize";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Fn {
                    params: &[
                        &TypeExpr {
                            kind: TypeKind::Builtin(types::BuiltinType::i32),
                            span: (3, 3).into()
                        },
                        &TypeExpr {
                            kind: TypeKind::Builtin(types::BuiltinType::u32),
                            span: (8, 3).into()
                        },
                    ],
                    generic_args: None,
                    ret: &TypeExpr {
                        kind: TypeKind::Builtin(types::BuiltinType::usize),
                        span: (13, 5).into()
                    }
                },
                span: (0, 18).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn fn_type_with_generic() {
        const SRC: &str = "fn[T](u32) usize";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Fn {
                    params: &[&TypeExpr {
                        kind: TypeKind::Builtin(types::BuiltinType::u32),
                        span: (6, 3).into()
                    },],
                    generic_args: Some(&[zeen_ast::declarations::GenericType {
                        name: rodeo.lock().unwrap().get_or_intern("T"),
                        interfaces: None,
                    }]),
                    ret: &TypeExpr {
                        kind: TypeKind::Builtin(types::BuiltinType::usize),
                        span: (11, 5).into()
                    }
                },
                span: (0, 16).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn fn_type_with_generic_and_interfaces() {
        const SRC: &str = "fn[T: Add + Display](u32) usize";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Fn {
                    params: &[&TypeExpr {
                        kind: TypeKind::Builtin(types::BuiltinType::u32),
                        span: (21, 3).into(),
                    }],
                    generic_args: Some(&[GenericType {
                        name: { rodeo.lock().unwrap().get_or_intern("T") },
                        interfaces: Some(&[
                            /*
                             * Kinda interesting bug:
                             *
                             * Mutex.lock() was locking interner inside until test scope end, but right after that
                             * we're trying to lock it again, and next "lockers" will wait previous user to go out scope.
                             *
                             * That means they never free the Mutex lock and get stuck in infinity loop.
                             *
                             * To fix this I've wrapped all calls in braces to create new local scopes and free the lock
                             * for other Mutex users.
                             *
                             * I've lost 2 hours of debugging for this...
                             */
                            { rodeo.lock().unwrap().get_or_intern("Add") },
                            { rodeo.lock().unwrap().get_or_intern("Display") },
                        ]),
                    }]),
                    ret: &TypeExpr {
                        kind: TypeKind::Builtin(types::BuiltinType::usize),
                        span: (26, 5).into(),
                    },
                },
                span: (0, 31).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn self_ref_type() {
        const SRC: &str = "self";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::SelfType,
                span: (0, 4).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn self_alias_type() {
        const SRC: &str = "Self";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::SelfAlias,
                span: (0, 4).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn const_type() {
        const SRC: &str = "const i32";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Const(&TypeExpr {
                    kind: TypeKind::Builtin(types::BuiltinType::i32),
                    span: (6, 3).into(),
                }),
                span: (0, 9).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn named_type() {
        const SRC: &str = "some_struct";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Named {
                    name: { rodeo.lock().unwrap().get_or_intern("some_struct") },
                    generic_args: None,
                },
                span: (0, 11).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }

    #[test]
    fn named_with_generic_args() {
        const SRC: &str = "some_struct[i32, u32]";

        make_type_parser!(SRC, tokens, bump, rodeo, parser, type_parser);

        assert_eq!(
            type_parser.parse().unwrap(),
            &TypeExpr {
                kind: TypeKind::Named {
                    name: { rodeo.lock().unwrap().get_or_intern("some_struct") },
                    generic_args: Some(&[
                        &TypeExpr {
                            kind: TypeKind::Builtin(types::BuiltinType::i32),
                            span: (12, 3).into(),
                        },
                        &TypeExpr {
                            kind: TypeKind::Builtin(types::BuiltinType::u32),
                            span: (17, 3).into(),
                        },
                    ]),
                },
                span: (0, 21).into()
            }
        );

        // eof
        assert_eq!(type_parser.parse(), None);
    }
}
