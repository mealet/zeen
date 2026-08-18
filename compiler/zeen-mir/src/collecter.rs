use std::rc::Rc;
use std::{cell::RefCell, collections::HashMap};

use lasso::Rodeo;
use zeen_hir::{
    HirModule,
    decl::{HirDecl, HirDeclKind, HirFn},
    stmt::{HirStmt, HirStmtKind},
};
use zeen_resolve::DefId;
use zeen_typecheck::result::TypeCheckResult;

use crate::ExternVarDecl;

pub fn collect_hir_fns(module: &HirModule) -> HashMap<DefId, Rc<HirFn>> {
    let mut map = HashMap::new();

    for decl in &module.decls {
        collect_from_decl(decl, &mut map);
    }

    map
}

fn collect_from_decl(decl: &HirDecl, map: &mut HashMap<DefId, Rc<HirFn>>) {
    match &decl.kind {
        HirDeclKind::Fn(f) => {
            map.insert(decl.def_id, f.clone());

            if let Some(body) = &f.body {
                collect_from_stmt(body, map);
            }
        }
        HirDeclKind::Struct(s) => {
            for method in &s.methods {
                collect_from_decl(method, map);
            }
        }
        HirDeclKind::Interface(i) => {
            for method in &i.methods {
                collect_from_decl(method, map);
            }
        }
        HirDeclKind::Implement(imp) => {
            for method in &imp.methods {
                collect_from_decl(method, map);
            }
        }
        HirDeclKind::Enum(_)
        | HirDeclKind::ExternVar { .. }
        | HirDeclKind::GlobalVar { .. }
        | HirDeclKind::ExternLink
        | HirDeclKind::ExternInclude => {}
    }
}

/// Walks a statement tree, collecting nested function declarations so they can
/// be monomorphized/lowered alongside top-level ones.
fn collect_from_stmt(stmt: &HirStmt, map: &mut HashMap<DefId, Rc<HirFn>>) {
    match &stmt.kind {
        HirStmtKind::FnDecl(decl) => {
            if let HirDeclKind::Fn(f) = &decl.kind {
                map.insert(decl.def_id, f.clone());

                if let Some(body) = &f.body {
                    collect_from_stmt(body, map);
                }
            }
        }

        HirStmtKind::Let { value, .. } => {
            if let Some(value) = value {
                collect_from_expr(value, map);
            }
        }

        HirStmtKind::Assign { object, value } => {
            collect_from_expr(object, map);
            collect_from_expr(value, map);
        }

        HirStmtKind::CompoundAssign { object, value, .. } => {
            collect_from_expr(object, map);
            collect_from_expr(value, map);
        }

        HirStmtKind::Return { value } => {
            if let Some(value) = value {
                collect_from_expr(value, map);
            }
        }

        HirStmtKind::While { block, .. } | HirStmtKind::For { block, .. } => {
            collect_from_stmt(block, map);
        }

        HirStmtKind::Expr(expr) => collect_from_expr(expr, map),

        HirStmtKind::Break | HirStmtKind::Continue | HirStmtKind::Error => {}
    }
}

fn collect_from_expr(expr: &zeen_hir::expr::HirExpr, map: &mut HashMap<DefId, Rc<HirFn>>) {
    use zeen_hir::expr::HirExprKind;

    match &expr.kind {
        HirExprKind::Block { stmts, trailing } => {
            for stmt in stmts {
                collect_from_stmt(stmt, map);
            }

            if let Some(trailing) = trailing {
                collect_from_expr(trailing, map);
            }
        }

        HirExprKind::Binary { lhs, rhs, .. } => {
            collect_from_expr(lhs, map);
            collect_from_expr(rhs, map);
        }

        HirExprKind::Unary { expr, .. } => collect_from_expr(expr, map),

        HirExprKind::Call { callee, args, .. } => {
            collect_from_expr(callee, map);

            for arg in args {
                collect_from_expr(arg, map);
            }
        }

        HirExprKind::MacroCall { args, .. } => {
            for arg in args {
                collect_from_expr(arg, map);
            }
        }

        HirExprKind::If {
            condition,
            then_block,
            else_block,
        } => {
            collect_from_expr(condition, map);
            collect_from_stmt(then_block, map);

            if let Some(else_block) = else_block {
                collect_from_stmt(else_block, map);
            }
        }

        HirExprKind::FieldAccess { object, .. } => collect_from_expr(object, map),

        HirExprKind::SliceAccess { object, index } => {
            collect_from_expr(object, map);
            collect_from_expr(index, map);
        }

        HirExprKind::StructInit { fields, .. } => {
            for field in fields {
                collect_from_expr(&field.value, map);
            }
        }

        HirExprKind::ArrayInit { elements } => {
            for element in elements {
                collect_from_expr(element, map);
            }
        }

        HirExprKind::Literal(_)
        | HirExprKind::VarRef(_)
        | HirExprKind::GenericParamRef(_)
        | HirExprKind::SelfValue(_)
        | HirExprKind::Type(_)
        | HirExprKind::Switch
        | HirExprKind::Error => {}
    }
}

pub fn collect_extern_vars(
    module: &HirModule,
    typecheck: &TypeCheckResult,
    rodeo: &Rc<RefCell<Rodeo>>,
) -> Vec<ExternVarDecl> {
    let mut out = Vec::new();

    for decl in &module.decls {
        if let HirDeclKind::ExternVar { name, .. } = &decl.kind {
            let symbol_name = rodeo.borrow().resolve(&name.0).to_string();

            let ty = typecheck
                .def_types
                .get(&decl.def_id)
                .copied()
                .expect("extern var must have a recorded type");

            out.push(ExternVarDecl { symbol_name, ty });
        }
    }

    out
}
