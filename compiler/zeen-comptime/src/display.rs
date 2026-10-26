use std::cell::RefCell;
use std::rc::Rc;

use lasso::{Rodeo, Spur};

use zeen_ast::{
    Expression, ExpressionKind,
    expressions::Literal,
    types::{TypeExpr, TypeKind},
};

pub(crate) fn interned(interner: &Rc<RefCell<Rodeo>>, spur: &Spur) -> String {
    interner.borrow().resolve(spur).to_string()
}

pub(crate) fn type_display(interner: &Rc<RefCell<Rodeo>>, ty: &TypeExpr<'_>) -> String {
    match &ty.kind {
        TypeKind::Builtin(builtin) => builtin.to_string(),

        TypeKind::SelfType | TypeKind::SelfAlias => "Self".to_string(),

        TypeKind::VaArgs => "...".to_string(),

        TypeKind::Named { name, generic_args } => match generic_args {
            Some(args) => {
                let rendered: Vec<String> =
                    args.iter().map(|arg| type_display(interner, arg)).collect();
                format!("{}[{}]", interned(interner, name), rendered.join(", "))
            }
            None => interned(interner, name),
        },

        TypeKind::Const(inner) => type_display(interner, inner),

        TypeKind::TypeOf(_) => "typeof".to_string(),

        TypeKind::Splice(expr) => format!("$({})", expr_display(interner, expr)),

        TypeKind::ComptimeCall(expr) => expr_display(interner, expr),

        TypeKind::SinglePointer(inner) => format!("*{}", type_display(interner, inner)),

        TypeKind::ManyPointer(inner) => format!("[*]{}", type_display(interner, inner)),

        TypeKind::Array { element, len } => match len {
            Some(len) => format!(
                "[{}]{}",
                expr_display(interner, len),
                type_display(interner, element)
            ),
            None => format!("[]{}", type_display(interner, element)),
        },

        TypeKind::Fn { params, ret, .. } => {
            let rendered: Vec<String> = params
                .iter()
                .map(|param| type_display(interner, param))
                .collect();
            format!(
                "fn({}) {}",
                rendered.join(", "),
                type_display(interner, ret)
            )
        }

        TypeKind::FatFn { params, ret, once } => {
            let rendered: Vec<String> = params
                .iter()
                .map(|param| type_display(interner, param))
                .collect();
            let name = if *once { "FnOnce" } else { "Fn" };
            format!(
                "{}({}) {}",
                name,
                rendered.join(", "),
                type_display(interner, ret)
            )
        }
    }
}

pub(crate) fn expr_display(interner: &Rc<RefCell<Rodeo>>, expr: &Expression<'_>) -> String {
    match &expr.kind {
        ExpressionKind::Literal(Literal::Int(value)) => value.to_string(),
        ExpressionKind::Ident { name, .. } => interned(interner, name),
        _ => "_".to_string(),
    }
}
