use std::collections::HashMap;
use std::rc::Rc;

use zeen_hir::{
    HirModule,
    decl::{HirDecl, HirDeclKind, HirFn},
};
use zeen_resolve::DefId;

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
