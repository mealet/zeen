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
