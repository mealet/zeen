use std::rc::Rc;
use std::{cell::RefCell, collections::HashMap};

use lasso::Rodeo;
use zeen_hir::{
    HirModule,
    decl::{HirDecl, HirDeclKind, HirFn},
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
        | HirDeclKind::ExternLink
        | HirDeclKind::ExternInclude => {}
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
