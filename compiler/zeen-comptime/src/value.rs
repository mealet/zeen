use std::cell::RefCell;
use std::rc::Rc;

use bumpalo::Bump;
use lasso::{Rodeo, Spur};
use smol_str::SmolStr;

use zeen_ast::{Declaration, Expression, ExpressionKind, TypeExpr, expressions::Literal};

#[derive(Clone, Debug, PartialEq)]
pub enum Value<'arena> {
    Int(i128),
    Float(f64),
    Bool(bool),
    Char(char),
    Str(Rc<str>),
    Array(Vec<Value<'arena>>),

    Type(&'arena TypeExpr<'arena>),
    Expr(&'arena Expression<'arena>),
    Decl(&'arena Declaration<'arena>),
}

impl Value<'_> {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Int(_) => "comptime_int",
            Self::Float(_) => "comptime_float",
            Self::Bool(_) => "comptime_bool",
            Self::Char(_) => "comptime_char",
            Self::Str(_) => "comptime_str",
            Self::Array(_) => "comptime_array",
            Self::Type(_) => "zeen_type",
            Self::Expr(_) => "zeen_expr",
            Self::Decl(_) => "zeen_decl",
        }
    }

    pub fn display(&self) -> String {
        match self {
            Self::Int(v) => v.to_string(),
            Self::Float(v) => v.to_string(),
            Self::Bool(v) => v.to_string(),
            Self::Char(_) => "char".to_string(),
            Self::Str(v) => v.to_string(),
            Self::Array(items) => {
                let mut s = String::from("[");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&item.display());
                }
                s.push(']');
                s
            }
            Self::Type(_) => "type".to_string(),
            Self::Expr(_) => "expr".to_string(),
            Self::Decl(_) => "decl".to_string(),
        }
    }
}

impl<'arena> Value<'arena> {
    pub fn into_expr(
        &self,
        arena: &'arena Bump,
        interner: &Rc<RefCell<Rodeo>>,
        span: miette::SourceSpan,
    ) -> Result<&'arena Expression<'arena>, SmolStr> {
        let kind = match self {
            Self::Int(v) => Literal::Int(
                i64::try_from(*v)
                    .map_err(|_| SmolStr::from("comptime integer does not fit into i64"))?,
            ),
            Self::Float(v) => Literal::Float(*v),
            Self::Bool(v) => Literal::Bool(*v),
            Self::Char(v) => Literal::Char(*v),
            Self::Str(v) => {
                let spur: Spur = interner.borrow_mut().get_or_intern(v.as_ref());
                Literal::String(spur)
            }
            Self::Array(items) => {
                let mut elements: Vec<&'arena Expression<'arena>> = Vec::with_capacity(items.len());
                for item in items.iter() {
                    let expr = item.into_expr(arena, interner, span)?;
                    elements.push(expr);
                }
                return Ok(arena.alloc(Expression {
                    kind: ExpressionKind::ArrayInit {
                        elements: arena.alloc_slice_copy(&elements),
                    },
                    span,
                }));
            }

            Self::Type(_) | Self::Expr(_) | Self::Decl(_) => {
                return Err(SmolStr::from(
                    "comptime fragment does not fit into a plain expression",
                ));
            }
        };

        Ok(arena.alloc(Expression {
            kind: ExpressionKind::Literal(kind),
            span,
        }))
    }
}
