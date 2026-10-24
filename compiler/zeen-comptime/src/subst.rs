use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use lasso::Spur;

use zeen_ast::{
    Declaration, DeclarationKind, Expression, ExpressionKind, Statement, StatementKind, TypeExpr,
    TypeKind,
    declarations::{AliasDecl, FnParam, GenericType},
    expressions::{Arm, FieldInit, Literal, Pattern},
};

use crate::error::Error;
use crate::interp::Interp;
use crate::value::Value;

impl<'arena> Interp<'arena> {
    pub(crate) fn substitute_program(
        &mut self,
        program: &'arena [&'arena Declaration<'arena>],
        module_of: &dyn Fn(&Declaration<'_>) -> PathBuf,
    ) -> Result<Vec<&'arena Declaration<'arena>>, Vec<Error>> {
        let mut out = Vec::with_capacity(program.len());

        for decl in program {
            let path = module_of(decl);
            self.default_src = Some(decl.source.src());

            match self.substitute_decl(&path, decl) {
                Ok(Some(replaced)) => out.push(replaced),
                Ok(None) => out.push(decl),
                Err(errors) => return Err(errors),
            }
        }

        if self.errors.is_empty() {
            Ok(out)
        } else {
            Err(self.errors.clone())
        }
    }

    fn substitute_decl(
        &mut self,
        module: &PathBuf,
        decl: &'arena Declaration<'arena>,
    ) -> Result<Option<&'arena Declaration<'arena>>, Vec<Error>> {
        match &decl.kind {
            DeclarationKind::GlobalVar {
                name,
                ty,
                value,
                is_const,
                is_pub,
            } => {
                self.push_shadow();

                let ty_result = self.substitute_type(module, ty);
                let value_result = self.substitute_expr(module, value);

                self.pop_shadow();

                match (ty_result, value_result) {
                    (Err(errors), _) | (_, Err(errors)) => Err(errors),
                    (Ok(None), Ok(None)) => Ok(None),
                    (Ok(new_ty), Ok(new_value)) => Ok(Some(self.arena.alloc(Declaration {
                        kind: DeclarationKind::GlobalVar {
                            name: *name,
                            ty: new_ty.unwrap_or(ty),
                            value: new_value.unwrap_or(value),
                            is_const: *is_const,
                            is_pub: *is_pub,
                        },
                        source: decl.source.clone(),
                    }))),
                }
            }

            DeclarationKind::FnDecl {
                name,
                generics,
                params,
                return_type,
                body,
                is_pub,
                is_extern,
            } => self.substitute_function(
                module,
                decl,
                *name,
                *generics,
                params,
                *return_type,
                *body,
                *is_pub,
                *is_extern,
            ),

            DeclarationKind::Alias(alias) => {
                self.push_shadow();

                let ty_result = self.substitute_type(module, alias.ty);

                self.pop_shadow();

                match ty_result {
                    Ok(None) => Ok(None),
                    Ok(Some(new_ty)) => {
                        let rebuilt_alias = AliasDecl {
                            name: alias.name,
                            is_pub: alias.is_pub,
                            generics: alias.generics,
                            ty: new_ty,
                        };

                        Ok(Some(self.arena.alloc(Declaration {
                            kind: DeclarationKind::Alias(rebuilt_alias),
                            source: decl.source.clone(),
                        })))
                    }
                    Err(errors) => Err(errors),
                }
            }

            DeclarationKind::StructDecl {
                name,
                is_pub,
                generics,
                fields,
                methods,
            } => {
                self.push_shadow();
                if let Some(generics) = generics {
                    for generic in generics.iter() {
                        self.bind_shadow(generic.name.0);
                    }
                }

                let mut new_fields: Vec<zeen_ast::declarations::StructField<'arena>> =
                    Vec::with_capacity(fields.len());
                let mut changed = false;

                for field in fields.iter() {
                    match self.substitute_type(module, field.ty) {
                        Ok(None) => new_fields.push(*field),
                        Ok(Some(new_ty)) => {
                            changed = true;
                            new_fields.push(zeen_ast::declarations::StructField {
                                name: field.name,
                                ty: new_ty,
                                is_pub: field.is_pub,
                            });
                        }
                        Err(errors) => {
                            self.pop_shadow();
                            return Err(errors);
                        }
                    }
                }

                let mut new_methods: Vec<&'arena Declaration<'arena>> =
                    Vec::with_capacity(methods.len());
                for method in methods.iter() {
                    match self.substitute_decl(module, method) {
                        Ok(None) => new_methods.push(*method),
                        Ok(Some(new_method)) => {
                            changed = true;
                            new_methods.push(new_method);
                        }
                        Err(errors) => {
                            self.pop_shadow();
                            return Err(errors);
                        }
                    }
                }

                self.pop_shadow();

                if changed {
                    let fields_alloc = self.arena.alloc_slice_copy(&new_fields);
                    let methods_alloc = self.arena.alloc_slice_copy(&new_methods);
                    Ok(Some(self.arena.alloc(Declaration {
                        kind: DeclarationKind::StructDecl {
                            name: *name,
                            is_pub: *is_pub,
                            generics: *generics,
                            fields: fields_alloc,
                            methods: methods_alloc,
                        },
                        source: decl.source.clone(),
                    })))
                } else {
                    Ok(None)
                }
            }

            DeclarationKind::EnumDecl {
                name,
                is_pub,
                generics,
                variants,
                methods,
            } => {
                self.push_shadow();
                if let Some(generics) = generics {
                    for generic in generics.iter() {
                        self.bind_shadow(generic.name.0);
                    }
                }

                let mut new_variants: Vec<zeen_ast::declarations::EnumVariant<'arena>> =
                    Vec::with_capacity(variants.len());
                let mut changed = false;

                for variant in variants.iter() {
                    let _new_payload = match &variant.payload {
                        Some(payload) => match payload {
                            zeen_ast::declarations::EnumVariantPayload::Single(t) => {
                                match self.substitute_type(module, t) {
                                    Ok(None) => new_variants.push(*variant),
                                    Ok(Some(new_ty)) => {
                                        changed = true;
                                        new_variants.push(zeen_ast::declarations::EnumVariant {
                                            name: variant.name,
                                            span: variant.span,
                                            payload: Some(
                                                zeen_ast::declarations::EnumVariantPayload::Single(
                                                    new_ty,
                                                ),
                                            ),
                                        });
                                    }
                                    Err(errors) => {
                                        self.pop_shadow();
                                        return Err(errors);
                                    }
                                }
                                continue;
                            }
                            zeen_ast::declarations::EnumVariantPayload::Anonymous(inner_fields) => {
                                let mut inner_changed = false;
                                let mut inner_fields_new: Vec<
                                    zeen_ast::declarations::StructField<'arena>,
                                > = Vec::with_capacity(inner_fields.len());

                                for field in inner_fields.iter() {
                                    match self.substitute_type(module, field.ty) {
                                        Ok(None) => inner_fields_new.push(*field),
                                        Ok(Some(new_ty)) => {
                                            inner_changed = true;
                                            inner_fields_new.push(
                                                zeen_ast::declarations::StructField {
                                                    name: field.name,
                                                    ty: new_ty,
                                                    is_pub: field.is_pub,
                                                },
                                            );
                                        }
                                        Err(errors) => {
                                            self.pop_shadow();
                                            return Err(errors);
                                        }
                                    }
                                }

                                if inner_changed {
                                    changed = true;
                                    new_variants.push(zeen_ast::declarations::EnumVariant {
                                        name: variant.name,
                                        span: variant.span,
                                        payload: Some(
                                            zeen_ast::declarations::EnumVariantPayload::Anonymous(
                                                self.arena.alloc_slice_copy(&inner_fields_new),
                                            ),
                                        ),
                                    });
                                } else {
                                    new_variants.push(*variant);
                                }
                                continue;
                            }
                        },
                        None => {
                            new_variants.push(*variant);
                            continue;
                        }
                    };
                }

                let mut new_methods: Vec<&'arena Declaration<'arena>> =
                    Vec::with_capacity(methods.len());
                for method in methods.iter() {
                    match self.substitute_decl(module, method) {
                        Ok(None) => new_methods.push(*method),
                        Ok(Some(new_method)) => {
                            changed = true;
                            new_methods.push(new_method);
                        }
                        Err(errors) => {
                            self.pop_shadow();
                            return Err(errors);
                        }
                    }
                }

                self.pop_shadow();

                if changed {
                    let variants_alloc = self.arena.alloc_slice_copy(&new_variants);
                    let methods_alloc = self.arena.alloc_slice_copy(&new_methods);
                    Ok(Some(self.arena.alloc(Declaration {
                        kind: DeclarationKind::EnumDecl {
                            name: *name,
                            is_pub: *is_pub,
                            generics: *generics,
                            variants: variants_alloc,
                            methods: methods_alloc,
                        },
                        source: decl.source.clone(),
                    })))
                } else {
                    Ok(None)
                }
            }

            DeclarationKind::ExternVar { name, ty, is_pub } => {
                self.push_shadow();

                let ty_result = self.substitute_type(module, ty);

                self.pop_shadow();

                match ty_result {
                    Ok(None) => Ok(None),
                    Ok(Some(new_ty)) => Ok(Some(self.arena.alloc(Declaration {
                        kind: DeclarationKind::ExternVar {
                            name: *name,
                            ty: new_ty,
                            is_pub: *is_pub,
                        },
                        source: decl.source.clone(),
                    }))),
                    Err(errors) => Err(errors),
                }
            }

            _ => Ok(None),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn substitute_function(
        &mut self,
        module: &PathBuf,
        decl: &'arena Declaration<'arena>,
        name: (Spur, miette::SourceSpan),
        generics: Option<&'arena [GenericType<'arena>]>,
        params: &'arena [FnParam<'arena>],
        return_type: Option<&'arena TypeExpr<'arena>>,
        body: Option<&'arena Statement<'arena>>,
        is_pub: bool,
        is_extern: bool,
    ) -> Result<Option<&'arena Declaration<'arena>>, Vec<Error>> {
        self.push_shadow();

        if let Some(generics) = generics {
            for generic in generics.iter() {
                self.bind_shadow(generic.name.0);
            }
        }

        let mut new_params: Vec<FnParam<'arena>> = Vec::with_capacity(params.len());
        let mut changed = false;

        for param in params.iter() {
            match self.substitute_type(module, param.ty) {
                Ok(None) => new_params.push(*param),
                Ok(Some(new_ty)) => {
                    changed = true;
                    new_params.push(FnParam {
                        name: param.name,
                        ty: new_ty,
                        span: param.span,
                    });
                }
                Err(errors) => {
                    self.pop_shadow();
                    return Err(errors);
                }
            }
        }

        for param in params.iter() {
            if let Some(param_name) = param.name {
                self.bind_shadow(param_name);
            }
        }

        let new_return = match return_type {
            Some(rt) => match self.substitute_type(module, rt) {
                Ok(None) => None,
                Ok(Some(new_rt)) => {
                    changed = true;
                    Some(new_rt)
                }
                Err(errors) => {
                    self.pop_shadow();
                    return Err(errors);
                }
            },
            None => None,
        };

        let new_body = match body {
            Some(body_stmt) => match self.substitute_statement(module, body_stmt) {
                Ok(None) => None,
                Ok(Some(new_stmt)) => {
                    changed = true;
                    Some(new_stmt)
                }
                Err(errors) => {
                    self.pop_shadow();
                    return Err(errors);
                }
            },
            None => None,
        };

        self.pop_shadow();

        if !changed {
            return Ok(None);
        }

        Ok(Some(self.arena.alloc(Declaration {
            kind: DeclarationKind::FnDecl {
                name,
                generics,
                params: self.arena.alloc_slice_copy(&new_params),
                return_type: new_return,
                body: new_body,
                is_pub,
                is_extern,
            },
            source: decl.source.clone(),
        })))
    }

    fn substitute_type(
        &mut self,
        module: &PathBuf,
        ty: &'arena TypeExpr<'arena>,
    ) -> Result<Option<&'arena TypeExpr<'arena>>, Vec<Error>> {
        match &ty.kind {
            TypeKind::Named { name, generic_args } => {
                if generic_args.is_none() && self.is_comptime_const(module, *name) {
                    match self.comptime_const_value(module, *name) {
                        Some(Value::Type(ty_expr)) => {
                            return Ok(Some(self.arena.alloc(*ty_expr)));
                        }
                        Some(_) => {}
                        None => {}
                    }
                }

                let _ = name;
                let Some(args) = generic_args else {
                    return Ok(None);
                };

                match self.substitute_type_args(module, args)? {
                    None => Ok(None),
                    Some(new_args) => {
                        let built = self.arena.alloc_slice_copy(&new_args);

                        let rebuilt = TypeExpr {
                            kind: TypeKind::Named {
                                name: *name,
                                generic_args: Some(built),
                            },
                            span: ty.span,
                        };

                        Ok(Some(self.arena.alloc(rebuilt)))
                    }
                }
            }

            TypeKind::Const(inner) => match self.substitute_type(module, inner)? {
                None => Ok(None),
                Some(new_inner) => {
                    let rebuilt = TypeExpr {
                        kind: TypeKind::Const(new_inner),
                        span: ty.span,
                    };

                    Ok(Some(self.arena.alloc(rebuilt)))
                }
            },

            TypeKind::TypeOf(expr) => match self.substitute_expr(module, expr)? {
                None => Ok(None),
                Some(new_expr) => {
                    let rebuilt = TypeExpr {
                        kind: TypeKind::TypeOf(new_expr),
                        span: ty.span,
                    };

                    Ok(Some(self.arena.alloc(rebuilt)))
                }
            },

            TypeKind::SinglePointer(inner) | TypeKind::ManyPointer(inner) => {
                let new_kind = match self.substitute_type(module, inner)? {
                    None => return Ok(None),
                    Some(new_inner) => match ty.kind {
                        TypeKind::SinglePointer(_) => TypeKind::SinglePointer(new_inner),
                        _ => TypeKind::ManyPointer(new_inner),
                    },
                };

                let rebuilt = TypeExpr {
                    kind: new_kind,
                    span: ty.span,
                };
                Ok(Some(self.arena.alloc(rebuilt)))
            }

            TypeKind::Array { element, len } => {
                let new_element = self.substitute_type(module, element)?;

                let new_len = match len {
                    Some(len_expr) => self.substitute_expr(module, len_expr)?,
                    None => None,
                };

                if new_element.is_none() && new_len.is_none() {
                    return Ok(None);
                }

                let rebuilt = TypeExpr {
                    kind: TypeKind::Array {
                        element: new_element.unwrap_or(element),
                        len: new_len.or(*len),
                    },
                    span: ty.span,
                };

                Ok(Some(self.arena.alloc(rebuilt)))
            }

            TypeKind::Fn {
                params,
                generic_args,
                ret,
            } => {
                let mut new_params: Vec<&'arena TypeExpr<'arena>> =
                    Vec::with_capacity(params.len());
                let mut changed = false;

                for param in params.iter() {
                    match self.substitute_type(module, param)? {
                        None => new_params.push(param),
                        Some(new_param) => {
                            changed = true;
                            new_params.push(new_param);
                        }
                    }
                }

                let new_ret = self.substitute_type(module, ret)?;

                if new_ret.is_some() {
                    changed = true;
                }

                if !changed {
                    return Ok(None);
                }

                let rebuilt = TypeExpr {
                    kind: TypeKind::Fn {
                        params: self.arena.alloc_slice_copy(&new_params),
                        generic_args: *generic_args,
                        ret: new_ret.unwrap_or(ret),
                    },
                    span: ty.span,
                };

                Ok(Some(self.arena.alloc(rebuilt)))
            }

            TypeKind::FatFn { params, ret, once } => {
                let mut new_params: Vec<&'arena TypeExpr<'arena>> =
                    Vec::with_capacity(params.len());
                let mut changed = false;

                for param in params.iter() {
                    match self.substitute_type(module, param)? {
                        None => new_params.push(param),
                        Some(new_param) => {
                            changed = true;
                            new_params.push(new_param);
                        }
                    }
                }

                let new_ret = self.substitute_type(module, ret)?;

                if new_ret.is_some() {
                    changed = true;
                }

                if !changed {
                    return Ok(None);
                }

                let rebuilt = TypeExpr {
                    kind: TypeKind::FatFn {
                        params: self.arena.alloc_slice_copy(&new_params),
                        ret: new_ret.unwrap_or(ret),
                        once: *once,
                    },
                    span: ty.span,
                };

                Ok(Some(self.arena.alloc(rebuilt)))
            }

            _ => Ok(None),
        }
    }

    fn substitute_type_args(
        &mut self,
        module: &PathBuf,
        args: &'arena [&'arena TypeExpr<'arena>],
    ) -> Result<Option<Vec<&'arena TypeExpr<'arena>>>, Vec<Error>> {
        let mut new_args: Vec<&'arena TypeExpr<'arena>> = Vec::with_capacity(args.len());
        let mut changed = false;

        for arg in args.iter() {
            match self.substitute_type(module, arg) {
                Ok(None) => new_args.push(arg),
                Ok(Some(new_arg)) => {
                    changed = true;
                    new_args.push(new_arg);
                }
                Err(errors) => return Err(errors),
            }
        }

        Ok(if changed { Some(new_args) } else { None })
    }

    fn substitute_expr(
        &mut self,
        module: &PathBuf,
        expr: &'arena Expression<'arena>,
    ) -> Result<Option<&'arena Expression<'arena>>, Vec<Error>> {
        self.substitute_expr_inner(module, expr)
    }

    fn substitute_expr_inner(
        &mut self,
        module: &PathBuf,
        expr: &'arena Expression<'arena>,
    ) -> Result<Option<&'arena Expression<'arena>>, Vec<Error>> {
        match &expr.kind {
            ExpressionKind::Literal(Literal::String(spur)) => {
                self.substitute_string_literal(module, *spur, expr.span)
            }

            ExpressionKind::Literal(_) => Ok(None),

            ExpressionKind::Ident { name, generic_args } => {
                if generic_args.is_some() {
                    return Ok(None);
                }

                if self.is_comptime_const(module, *name) {
                    match self.comptime_const_value(module, *name) {
                        Some(value) => match self.value_into_expr(value, expr.span) {
                            Ok(new_expr) => Ok(Some(self.arena.alloc(new_expr))),
                            Err(error) => Err(self.report(error)),
                        },
                        None => Err(self.errors.clone()),
                    }
                } else {
                    Ok(None)
                }
            }

            ExpressionKind::Binary { lhs, rhs, op } => {
                let lhs_new = self.substitute_expr(module, lhs);
                let rhs_new = self.substitute_expr(module, rhs);

                match (lhs_new, rhs_new) {
                    (Err(errors), _) | (_, Err(errors)) => Err(errors),
                    (Ok(None), Ok(None)) => Ok(None),
                    (Ok(new_lhs), Ok(new_rhs)) => Ok(Some(self.arena.alloc(Expression {
                        kind: ExpressionKind::Binary {
                            lhs: new_lhs.unwrap_or(lhs),
                            rhs: new_rhs.unwrap_or(rhs),
                            op: *op,
                        },
                        span: expr.span,
                    }))),
                }
            }

            ExpressionKind::Unary { expr: inner, op } => {
                match self.substitute_expr(module, inner)? {
                    None => Ok(None),
                    Some(new_inner) => Ok(Some(self.arena.alloc(Expression {
                        kind: ExpressionKind::Unary {
                            expr: new_inner,
                            op: *op,
                        },
                        span: expr.span,
                    }))),
                }
            }

            ExpressionKind::Call { callee, args } => {
                let original_callee_is_comptime_fn = matches!(&callee.kind, ExpressionKind::Ident { name, generic_args: None } if self.is_comptime_fn(module, *name));

                let callee_new = self.substitute_expr(module, callee)?;

                let mut new_args: Vec<&'arena Expression<'arena>> = Vec::with_capacity(args.len());
                for arg in args.iter() {
                    match self.substitute_expr(module, arg) {
                        Ok(None) => new_args.push(arg),
                        Ok(Some(new_arg)) => new_args.push(new_arg),
                        Err(errors) => return Err(errors),
                    }
                }

                let args_any_changed = new_args
                    .iter()
                    .zip(args.iter())
                    .any(|(a, b)| !std::ptr::eq(*a as *const _, (*b) as *const _));
                let any_changed = args_any_changed || callee_new.is_some();

                if original_callee_is_comptime_fn {
                    let callee_name = match &callee.kind {
                        ExpressionKind::Ident { name, .. } => *name,
                        _ => unreachable!("original_callee_is_comptime_fn"),
                    };

                    let mut values: Vec<Value<'arena>> = Vec::with_capacity(new_args.len());

                    for arg in new_args.iter() {
                        match self.eval_fresh_expr(module, arg) {
                            Ok(value) => values.push(value),
                            Err(_) => return Err(self.errors.clone()),
                        }
                    }

                    let is_void_fn = self.scopes.get(module)
                        .and_then(|scope| scope.fns.get(&callee_name))
                        .map(|decl| {
                            matches!(&decl.kind, DeclarationKind::ComptimeFn { return_type: Some(rt), .. } if matches!(&rt.kind, TypeKind::Builtin(zt) if matches!(zt, zeen_ast::types::BuiltinType::void)))
                        })
                        .unwrap_or(false);

                    if is_void_fn {
                        let _ = self.eval_comptime_call(module, callee_name, expr.span, &values);
                        let spur = self.interner.borrow_mut().get_or_intern("void");
                        let void_macro = Expression {
                            kind: ExpressionKind::MacroCall {
                                name: (spur, expr.span),
                                args: self.arena.alloc_slice_copy(&[]),
                            },
                            span: expr.span,
                        };
                        return Ok(Some(self.arena.alloc(void_macro)));
                    }

                    return match self.eval_comptime_call(module, callee_name, expr.span, &values) {
                        Ok(value) => match self.value_into_expr(value, expr.span) {
                            Ok(new_call_expr) => Ok(Some(self.arena.alloc(new_call_expr))),
                            Err(error) => Err(self.report(error)),
                        },
                        Err(_) => Err(self.errors.clone()),
                    };
                }

                if !any_changed {
                    return Ok(None);
                }

                Ok(Some(self.arena.alloc(Expression {
                    kind: ExpressionKind::Call {
                        callee: callee_new.unwrap_or(callee),
                        args: self.arena.alloc_slice_copy(&new_args),
                    },
                    span: expr.span,
                })))
            }

            ExpressionKind::MacroCall { name, args } => {
                let mut new_args: Vec<&'arena Expression<'arena>> = Vec::with_capacity(args.len());
                let mut changed = false;

                for arg in args.iter() {
                    match self.substitute_expr(module, arg)? {
                        None => new_args.push(arg),
                        Some(new_arg) => {
                            changed = true;
                            new_args.push(new_arg);
                        }
                    }
                }

                if !changed {
                    return Ok(None);
                }

                Ok(Some(self.arena.alloc(Expression {
                    kind: ExpressionKind::MacroCall {
                        name: *name,
                        args: self.arena.alloc_slice_copy(&new_args),
                    },
                    span: expr.span,
                })))
            }

            ExpressionKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let cond_new = self.substitute_expr(module, condition);

                self.push_shadow();
                let then_new = match cond_new {
                    Err(errors) => {
                        self.pop_shadow();
                        return Err(errors);
                    }
                    _ => self.substitute_statement(module, then_block),
                };

                let else_new = match then_new {
                    Err(errors) => {
                        self.pop_shadow();
                        return Err(errors);
                    }
                    Ok(_) => match else_block {
                        Some(else_stmt) => self.substitute_statement(module, else_stmt),
                        None => Ok(None),
                    },
                };

                self.pop_shadow();

                match (cond_new, then_new, else_new) {
                    (Err(errors), _, _) => Err(errors),
                    (_, Err(errors), _) => Err(errors),
                    (_, _, Err(errors)) => Err(errors),
                    (Ok(None), Ok(None), Ok(None)) => Ok(None),
                    (Ok(new_cond), Ok(new_then), Ok(new_else)) => {
                        Ok(Some(self.arena.alloc(Expression {
                            kind: ExpressionKind::If {
                                condition: new_cond.unwrap_or(condition),
                                then_block: new_then.unwrap_or(then_block),
                                else_block: new_else.or(*else_block),
                            },
                            span: expr.span,
                        })))
                    }
                }
            }

            ExpressionKind::Switch { object, arms } => {
                let object_new = self.substitute_expr(module, object);

                let mut new_arms: Vec<&'arena Arm<'arena>> = Vec::with_capacity(arms.len());
                let mut changed = false;

                for arm in arms.iter() {
                    self.push_shadow();

                    let arm_result = self.substitute_arm(module, arm);

                    self.pop_shadow();

                    match arm_result {
                        Ok(None) => new_arms.push(arm),
                        Ok(Some(new_arm)) => {
                            changed = true;
                            new_arms.push(self.arena.alloc(new_arm));
                        }
                        Err(errors) => return Err(errors),
                    }
                }

                match (object_new, changed) {
                    (Err(errors), _) => Err(errors),
                    (Ok(None), false) => Ok(None),
                    (Ok(new_object), _) => {
                        let rebuilt = Expression {
                            kind: ExpressionKind::Switch {
                                object: new_object.unwrap_or(object),
                                arms: self.arena.alloc_slice_copy(&new_arms),
                            },
                            span: expr.span,
                        };

                        Ok(Some(self.arena.alloc(rebuilt)))
                    }
                }
            }

            ExpressionKind::FieldAccess { object, field } => {
                match self.substitute_expr(module, object)? {
                    None => Ok(None),
                    Some(new_object) => Ok(Some(self.arena.alloc(Expression {
                        kind: ExpressionKind::FieldAccess {
                            object: new_object,
                            field,
                        },
                        span: expr.span,
                    }))),
                }
            }

            ExpressionKind::SliceAccess { object, index } => {
                let object_new = self.substitute_expr(module, object)?;
                let index_new = self.substitute_expr(module, index)?;

                if object_new.is_none() && index_new.is_none() {
                    return Ok(None);
                }

                let new_expr = self.arena.alloc(Expression {
                    kind: ExpressionKind::SliceAccess {
                        object: object_new.unwrap_or(object),
                        index: index_new.unwrap_or(index),
                    },
                    span: expr.span,
                });

                if let Ok(value) = self.eval_expr(new_expr)
                    && let Ok(literal_expr) = self.value_into_expr(value, expr.span)
                {
                    return Ok(Some(self.arena.alloc(literal_expr)));
                }

                Ok(Some(new_expr))
            }

            ExpressionKind::StructInit { ty, fields } => {
                let ty_new = self.substitute_expr(module, ty)?;

                let mut new_fields: Vec<FieldInit<'arena>> =
                    Vec::with_capacity(fields.map(|fields| fields.len()).unwrap_or(0));
                let mut changed = false;

                if let Some(fields) = fields {
                    for field in fields.iter() {
                        match self.substitute_expr(module, field.value) {
                            Ok(None) => new_fields.push(*field),
                            Ok(Some(new_value)) => {
                                changed = true;
                                new_fields.push(FieldInit {
                                    name: field.name,
                                    value: new_value,
                                    span: field.span,
                                });
                            }
                            Err(errors) => return Err(errors),
                        }
                    }
                }

                if ty_new.is_none() && !changed {
                    return Ok(None);
                }

                let rebuilt = Expression {
                    kind: ExpressionKind::StructInit {
                        ty: ty_new.unwrap_or(ty),
                        fields: match fields {
                            None => None,
                            Some(_) => Some(self.arena.alloc_slice_copy(&new_fields)),
                        },
                    },
                    span: expr.span,
                };

                Ok(Some(self.arena.alloc(rebuilt)))
            }

            ExpressionKind::ArrayInit { elements } => {
                self.substitute_expr_list(module, elements, |arena, list| Expression {
                    kind: ExpressionKind::ArrayInit {
                        elements: arena.alloc_slice_copy(&list),
                    },
                    span: expr.span,
                })
            }

            ExpressionKind::ArrayRepeatInit { element, len } => {
                let element_new = self.substitute_expr(module, element)?;
                let len_new = self.substitute_expr(module, len)?;

                if element_new.is_none() && len_new.is_none() {
                    return Ok(None);
                }

                Ok(Some(self.arena.alloc(Expression {
                    kind: ExpressionKind::ArrayRepeatInit {
                        element: element_new.unwrap_or(element),
                        len: len_new.unwrap_or(len),
                    },
                    span: expr.span,
                })))
            }

            ExpressionKind::Block { stmts, trailing } => {
                self.push_shadow();

                let mut new_stmts: Vec<&'arena Statement<'arena>> = Vec::with_capacity(stmts.len());
                let mut changed = false;

                for stmt in stmts.iter() {
                    match self.substitute_statement(module, stmt) {
                        Ok(None) => new_stmts.push(stmt),
                        Ok(Some(new_stmt)) => {
                            changed = true;
                            new_stmts.push(new_stmt);
                        }
                        Err(errors) => {
                            self.pop_shadow();
                            return Err(errors);
                        }
                    }
                }

                let trailing_new = match trailing {
                    Some(trailing_expr) => self.substitute_expr(module, trailing_expr),
                    None => Ok(None),
                };

                self.pop_shadow();

                match trailing_new {
                    Err(errors) => Err(errors),
                    Ok(None) if !changed => Ok(None),
                    Ok(new_trailing) => {
                        let rebuilt = Expression {
                            kind: ExpressionKind::Block {
                                stmts: self.arena.alloc_slice_copy(&new_stmts),
                                trailing: new_trailing.or(*trailing),
                            },
                            span: expr.span,
                        };

                        Ok(Some(self.arena.alloc(rebuilt)))
                    }
                }
            }

            ExpressionKind::Closure {
                params,
                return_type,
                body,
            } => {
                self.push_shadow();

                for param in params.iter() {
                    if let Some(name) = param.name {
                        self.bind_shadow(name);
                    }
                }

                let mut new_params: Vec<FnParam<'arena>> = Vec::with_capacity(params.len());
                let mut changed = false;

                for param in params.iter() {
                    match self.substitute_type(module, param.ty) {
                        Ok(None) => new_params.push(*param),
                        Ok(Some(new_ty)) => {
                            changed = true;
                            new_params.push(FnParam {
                                name: param.name,
                                ty: new_ty,
                                span: param.span,
                            });
                        }
                        Err(errors) => {
                            self.pop_shadow();
                            return Err(errors);
                        }
                    }
                }

                let mut new_return = *return_type;

                let new_return_type = match return_type {
                    Some(rt) => match self.substitute_type(module, rt) {
                        Ok(None) => Ok(None),
                        Ok(Some(new_rt)) => {
                            changed = true;
                            new_return = Some(new_rt);
                            Ok(Some(new_rt))
                        }
                        Err(errors) => {
                            self.pop_shadow();
                            Err(errors)
                        }
                    },
                    None => Ok(None),
                };

                let body_result = match new_return_type {
                    Err(errors) => Err(errors),
                    Ok(_) => match self.substitute_statement(module, body) {
                        Ok(None) => Ok(None),
                        Ok(Some(new_stmt)) => {
                            changed = true;
                            Ok(Some(new_stmt))
                        }
                        Err(errors) => {
                            self.pop_shadow();
                            Err(errors)
                        }
                    },
                };

                self.pop_shadow();

                let new_body = body_result?;

                let _ = new_return;

                if !changed {
                    return Ok(None);
                }

                Ok(Some(self.arena.alloc(Expression {
                    kind: ExpressionKind::Closure {
                        params: self.arena.alloc_slice_copy(&new_params),
                        return_type: new_return,
                        body: new_body.unwrap_or(body),
                    },
                    span: expr.span,
                })))
            }

            ExpressionKind::Type(ty) => match self.substitute_type(module, ty)? {
                None => Ok(None),
                Some(new_ty) => Ok(Some(self.arena.alloc(Expression {
                    kind: ExpressionKind::Type(new_ty),
                    span: expr.span,
                }))),
            },

            ExpressionKind::Range {
                start,
                end,
                inclusive,
            } => {
                let start_new = match start {
                    Some(start_expr) => self.substitute_expr(module, start_expr)?,
                    None => None,
                };

                let end_new = match end {
                    Some(end_expr) => self.substitute_expr(module, end_expr)?,
                    None => None,
                };

                if start_new.is_none() && end_new.is_none() {
                    return Ok(None);
                }

                Ok(Some(self.arena.alloc(Expression {
                    kind: ExpressionKind::Range {
                        start: start_new.or(*start),
                        end: end_new.or(*end),
                        inclusive: *inclusive,
                    },
                    span: expr.span,
                })))
            }

            ExpressionKind::TargetVar(_) => Ok(None),

            ExpressionKind::Splice { expr } => match self.eval_fresh_expr(module, expr) {
                Ok(value) => match self.value_into_expr(value, expr.span) {
                    Ok(new_expr) => Ok(Some(self.arena.alloc(new_expr))),
                    Err(error) => Err(self.report(error)),
                },
                Err(_) => Err(self.errors.clone()),
            },

            other => {
                let _ = other;
                Ok(None)
            }
        }
    }

    fn substitute_arm(
        &mut self,
        module: &PathBuf,
        arm: &'arena Arm<'arena>,
    ) -> Result<Option<Arm<'arena>>, Vec<Error>> {
        let new_pattern = self.substitute_pattern(module, &arm.pattern)?;

        let guard_new = match arm.guard {
            Some(guard) => self.substitute_expr(module, guard)?,
            None => None,
        };

        let body_new = self.substitute_expr(module, arm.body)?;

        if new_pattern.is_none() && guard_new.is_none() && body_new.is_none() {
            return Ok(None);
        }

        Ok(Some(Arm {
            pattern: new_pattern.unwrap_or(arm.pattern),
            body: body_new.unwrap_or(arm.body),
            guard: guard_new.or(arm.guard),
        }))
    }

    fn substitute_pattern(
        &mut self,
        module: &PathBuf,
        pattern: &Pattern<'arena>,
    ) -> Result<Option<Pattern<'arena>>, Vec<Error>> {
        match pattern {
            Pattern::Named { name, span } => {
                if self.is_comptime_const(module, *name) {
                    return match self.comptime_const_value(module, *name) {
                        Some(value) => match self.value_into_pattern(value, *span) {
                            Ok(new_pattern) => Ok(Some(new_pattern)),
                            Err(error) => Err(self.report(error)),
                        },
                        None => Err(self.errors.clone()),
                    };
                }

                Ok(None)
            }
            Pattern::Or(patterns) => {
                let mut new_patterns: Vec<Pattern<'arena>> = Vec::with_capacity(patterns.len());
                let mut changed = false;

                for inner in patterns.iter() {
                    match self.substitute_pattern(module, inner) {
                        Ok(None) => new_patterns.push(*inner),
                        Ok(Some(new_inner)) => {
                            changed = true;
                            new_patterns.push(new_inner);
                        }
                        Err(errors) => return Err(errors),
                    }
                }

                if !changed {
                    return Ok(None);
                }

                Ok(Some(Pattern::Or(
                    self.arena.alloc_slice_copy(&new_patterns),
                )))
            }
            _ => Ok(None),
        }
    }

    fn substitute_statement(
        &mut self,
        module: &PathBuf,
        stmt: &'arena Statement<'arena>,
    ) -> Result<Option<&'arena Statement<'arena>>, Vec<Error>> {
        match &stmt.kind {
            StatementKind::Let {
                name,
                explicit_type,
                value,
                is_const,
            } => {
                let new_type = match explicit_type {
                    Some(ty) => self.substitute_type(module, ty)?,
                    None => None,
                };

                let new_value = match value {
                    Some(value_expr) => self.substitute_expr(module, value_expr)?,
                    None => None,
                };

                self.bind_shadow(*name);

                let _ = is_const;

                if new_type.is_none() && new_value.is_none() {
                    return Ok(None);
                }

                Ok(Some(self.arena.alloc(Statement {
                    kind: StatementKind::Let {
                        name: *name,
                        explicit_type: new_type.or(*explicit_type),
                        value: new_value.or(*value),
                        is_const: *is_const,
                    },
                    span: stmt.span,
                })))
            }

            StatementKind::Assign { object, value } => {
                let object_new = self.substitute_expr(module, object)?;
                let value_new = self.substitute_expr(module, value)?;

                if object_new.is_none() && value_new.is_none() {
                    return Ok(None);
                }

                Ok(Some(self.arena.alloc(Statement {
                    kind: StatementKind::Assign {
                        object: object_new.unwrap_or(object),
                        value: value_new.unwrap_or(value),
                    },
                    span: stmt.span,
                })))
            }

            StatementKind::CompoundAssign { object, value, op } => {
                let object_new = self.substitute_expr(module, object)?;
                let value_new = self.substitute_expr(module, value)?;

                if object_new.is_none() && value_new.is_none() {
                    return Ok(None);
                }

                Ok(Some(self.arena.alloc(Statement {
                    kind: StatementKind::CompoundAssign {
                        object: object_new.unwrap_or(object),
                        value: value_new.unwrap_or(value),
                        op: *op,
                    },
                    span: stmt.span,
                })))
            }

            StatementKind::Return { value } => {
                let new_value = match value {
                    Some(value_expr) => self.substitute_expr(module, value_expr)?,
                    None => None,
                };

                if new_value.is_none() {
                    return Ok(None);
                }

                Ok(Some(self.arena.alloc(Statement {
                    kind: StatementKind::Return { value: new_value },
                    span: stmt.span,
                })))
            }

            StatementKind::While { condition, block } => {
                let cond_new = self.substitute_expr(module, condition)?;

                self.push_shadow();
                let block_new = self.substitute_statement(module, block);
                self.pop_shadow();

                match block_new {
                    Err(errors) => Err(errors),
                    Ok(None) if cond_new.is_none() => Ok(None),
                    Ok(new_block) => Ok(Some(self.arena.alloc(Statement {
                        kind: StatementKind::While {
                            condition: cond_new.unwrap_or(condition),
                            block: new_block.unwrap_or(block),
                        },
                        span: stmt.span,
                    }))),
                }
            }

            StatementKind::For {
                varname,
                iterator,
                block,
            } => {
                let iter_new = self.substitute_expr(module, iterator)?;

                self.push_shadow();
                self.bind_shadow(varname.0);
                let block_new = self.substitute_statement(module, block);
                self.pop_shadow();

                match block_new {
                    Err(errors) => Err(errors),
                    Ok(None) if iter_new.is_none() => Ok(None),
                    Ok(new_block) => Ok(Some(self.arena.alloc(Statement {
                        kind: StatementKind::For {
                            varname: *varname,
                            iterator: iter_new.unwrap_or(iterator),
                            block: new_block.unwrap_or(block),
                        },
                        span: stmt.span,
                    }))),
                }
            }

            StatementKind::Expr(expr) | StatementKind::TrailingExpr(expr) => {
                match self.substitute_expr(module, expr)? {
                    None => Ok(None),
                    Some(new_expr) => {
                        let kind_to_use = match stmt.kind {
                            StatementKind::Expr(_) => StatementKind::Expr(new_expr),
                            _ => StatementKind::TrailingExpr(new_expr),
                        };

                        Ok(Some(self.arena.alloc(Statement {
                            kind: kind_to_use,
                            span: stmt.span,
                        })))
                    }
                }
            }

            StatementKind::FnDecl(nested) => self.substitute_decl(module, nested).map(|result| {
                result.map(|new_decl| {
                    let rebuilt: &'arena Statement<'arena> = self.arena.alloc(Statement {
                        kind: StatementKind::FnDecl(new_decl),
                        span: stmt.span,
                    });

                    rebuilt
                })
            }),

            StatementKind::Break | StatementKind::Continue => Ok(None),

            other => {
                let _ = other;
                Ok(None)
            }
        }
    }

    fn substitute_expr_list(
        &mut self,
        module: &PathBuf,
        elements: &'arena [&'arena Expression<'arena>],
        build: impl FnOnce(&'arena bumpalo::Bump, Vec<&'arena Expression<'arena>>) -> Expression<'arena>,
    ) -> Result<Option<&'arena Expression<'arena>>, Vec<Error>> {
        let mut new_elements: Vec<&'arena Expression<'arena>> = Vec::with_capacity(elements.len());
        let mut changed = false;

        for element in elements.iter() {
            match self.substitute_expr(module, element) {
                Ok(None) => new_elements.push(element),
                Ok(Some(new_element)) => {
                    changed = true;
                    new_elements.push(new_element);
                }
                Err(errors) => return Err(errors),
            }
        }

        if !changed {
            return Ok(None);
        }

        let expr = build(self.arena, new_elements);
        Ok(Some(self.arena.alloc(expr)))
    }

    fn eval_fresh_expr(
        &mut self,
        module: &PathBuf,
        expr: &'arena Expression<'arena>,
    ) -> crate::interp::EvalResult<'arena> {
        self.frames.push(crate::interp::CallFrame {
            module: module.clone(),
            src: self.scope_src(module),
            scopes: vec![rustc_hash::FxHashMap::default()],
            caller_name: None,
        });

        let result = self.eval_expr(expr);

        self.frames.pop();

        result
    }

    fn substitute_string_literal(
        &mut self,
        module: &PathBuf,
        spur: Spur,
        span: miette::SourceSpan,
    ) -> Result<Option<&'arena Expression<'arena>>, Vec<Error>> {
        let text = self.interned(&spur);

        if !text.contains("$(") {
            return Ok(None);
        }

        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut changed = false;
        let mut i = 0;

        while i < chars.len() {
            if chars[i] == '$' && chars.get(i + 1) == Some(&'(') {
                let start = i + 2;
                let mut depth = 1usize;
                let mut j = start;

                while j < chars.len() {
                    match chars[j] {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }

                    j += 1;
                }

                if depth != 0 {
                    self.errors.push(Error::TypeMismatch {
                        message: "unterminated `$(` in string literal".into(),
                        src: self.current_src(),
                        span,
                    });
                    return Err(self.errors.clone());
                }

                let fragment: String = chars[start..j].iter().collect();
                let value = self.eval_interpolation(module, &fragment, span)?;
                out.push_str(&value.display());
                changed = true;
                i = j + 1;
                continue;
            }

            out.push(chars[i]);
            i += 1;
        }

        if !changed {
            return Ok(None);
        }

        let interned = self.interner.borrow_mut().get_or_intern(&out);
        let literal = self.arena.alloc(Expression {
            kind: ExpressionKind::Literal(Literal::String(interned)),
            span,
        });

        Ok(Some(literal))
    }

    fn eval_interpolation(
        &mut self,
        module: &PathBuf,
        fragment: &str,
        span: miette::SourceSpan,
    ) -> Result<Value<'arena>, Vec<Error>> {
        let src = Arc::new(fragment.to_string());
        let filename = Rc::new(String::from("<comptime interpolation>"));
        let tokens: Vec<_> = zeen_lexer::tokenize(fragment).collect();
        let mut iter = tokens.into_iter();

        let mut parser = zeen_parser::Parser::new(
            filename,
            src,
            &mut iter,
            self.arena,
            Rc::clone(&self.interner),
        );
        let parsed = zeen_parser::expressions::ExprParser::new(&mut parser).parse();

        let expr = match parsed {
            Some(expr) if parser.errors.is_empty() => expr,
            _ => {
                self.errors.push(Error::TypeMismatch {
                    message: "invalid expression inside `$(` splice".into(),
                    src: self.current_src(),
                    span,
                });
                return Err(self.errors.clone());
            }
        };

        match self.eval_fresh_expr(module, expr) {
            Ok(value) => Ok(value),
            Err(_) => Err(self.errors.clone()),
        }
    }

    pub(crate) fn value_into_expr(
        &self,
        value: Value<'arena>,
        span: miette::SourceSpan,
    ) -> std::result::Result<Expression<'arena>, Error> {
        let literal = match value {
            Value::Int(value) => {
                if value < i64::MIN as i128 || value > i64::MAX as i128 {
                    return Err(Error::TypeMismatch {
                        message: format!(
                            "comptime integer value `{value}` is out of literal range"
                        )
                        .into(),
                        src: self.current_src(),
                        span,
                    });
                }

                zeen_ast::expressions::Literal::Int(value as i64)
            }
            Value::Float(value) => zeen_ast::expressions::Literal::Float(value),
            Value::Bool(value) => zeen_ast::expressions::Literal::Bool(value),
            Value::Char(value) => zeen_ast::expressions::Literal::Char(value),
            Value::Str(value) => {
                let interned = self.interner.borrow_mut().get_or_intern(&value);
                zeen_ast::expressions::Literal::String(interned)
            }

            Value::Type(ty) => {
                return Ok(Expression {
                    kind: ExpressionKind::Type(ty),
                    span,
                });
            }

            Value::Expr(expr) => {
                return Ok(*expr);
            }

            Value::Decl(_) => {
                return Err(Error::TypeMismatch {
                    message: "comptime declaration values are not valid runtime expressions in v1"
                        .into(),
                    src: self.current_src(),
                    span,
                });
            }

            Value::Array(items) => {
                let mut elements: Vec<&'arena Expression<'arena>> = Vec::with_capacity(items.len());

                for item in items {
                    let element = self.value_into_expr(item, span)?;
                    elements.push(self.arena.alloc(element));
                }

                return Ok(Expression {
                    kind: ExpressionKind::ArrayInit {
                        elements: self.arena.alloc_slice_copy(&elements),
                    },
                    span,
                });
            }
        };

        Ok(Expression {
            kind: ExpressionKind::Literal(literal),
            span,
        })
    }

    pub(crate) fn value_into_pattern(
        &self,
        value: Value<'arena>,
        span: miette::SourceSpan,
    ) -> std::result::Result<Pattern<'arena>, Error> {
        let literal = match value {
            Value::Int(value) => zeen_ast::expressions::Literal::Int(value as i64),
            Value::Float(value) => zeen_ast::expressions::Literal::Float(value),
            Value::Bool(value) => zeen_ast::expressions::Literal::Bool(value),
            Value::Char(value) => zeen_ast::expressions::Literal::Char(value),
            Value::Str(value) => {
                let interned = self.interner.borrow_mut().get_or_intern(&value);
                zeen_ast::expressions::Literal::String(interned)
            }
            _ => {
                return Err(Error::TypeMismatch {
                    message: "comptime type values are not valid runtime values in v1".into(),
                    src: self.current_src(),
                    span,
                });
            }
        };

        Ok(Pattern::Literal(literal))
    }
}
