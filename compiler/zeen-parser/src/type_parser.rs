use crate::{Parser, error::ParserError};

use zeen_ast::types::{self, TypeExpr, TypeKind};
use zeen_lexer::{Token, TokenKind, token};

use smallvec::SmallVec;

pub struct TypeParser<'ctx, 'pr> {
    p: &'pr mut Parser<'ctx>,
}

impl<'ctx, 'pr> TypeParser<'ctx, 'pr> {
    pub fn new(parser: &'pr mut Parser<'ctx>) -> Self {
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

impl<'ctx, 'pr> TypeParser<'ctx, 'pr> {
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

        let mut generic_args: Option<&'_ [&'_ zeen_ast::TypeExpr<'_>]> = None;

        if self.p.eat(TokenKind::OpenBracket) {
            let mut args_buffer: SmallVec<[&zeen_ast::TypeExpr<'_>; 16]> = SmallVec::new();

            while self.p.current().kind != TokenKind::Eof {
                if self.p.eat(TokenKind::CloseBracket) {
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

        let _ = self.p.advance()?;

        let expr = self.p.arena.alloc(TypeExpr {
            kind: TypeKind::SelfType,
            span: kw_span,
        });

        Some(expr)
    }

    fn parse_self_alias(&mut self) -> Option<&'ctx TypeExpr<'ctx>> {
        let kw_self_upper = self.p.current();
        let kw_span = kw_self_upper.span;

        let _ = self.p.advance()?;

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
        let _ = self.p.advance()?;

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
            kind: TypeKind::Pointer(child),
            span: kw_const.merge_span(child.span),
        });

        Some(expr)
    }
}
