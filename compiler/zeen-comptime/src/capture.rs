use lasso::Spur;
use std::path::PathBuf;

use zeen_ast::{
    Declaration, DeclarationKind, Expression, ExpressionKind, Statement, StatementKind, TypeExpr,
    TypeKind,
    declarations::{EnumVariantPayload, FnParam, StructField},
    expressions::{Arm, FieldInit},
};

use crate::error::Error;
use crate::interp::Interp;
use crate::value::Value;

pub(crate) const CAPTURE_DEPTH_LIMIT: usize = 64;

impl<'arena> Interp<'arena> {
    pub(crate) fn capture_expr(
        &mut self,
        module: &PathBuf,
        expr: &'arena Expression<'arena>,
    ) -> Result<&'arena Expression<'arena>, ()> {
        if let ExpressionKind::Splice { expr: inner } = &expr.kind {
            return self.splice_expr(module, inner, expr.span);
        }

        match expr.kind {
            ExpressionKind::Literal(_)
            | ExpressionKind::Ident { .. }
            | ExpressionKind::TargetVar(_) => Ok(expr),

            ExpressionKind::Binary { lhs, rhs, op } => {
                let new_lhs = self.capture_expr(module, lhs)?;
                let new_rhs = self.capture_expr(module, rhs)?;

                if unchanged(lhs, new_lhs) && unchanged(rhs, new_rhs) {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::Binary {
                        lhs: new_lhs,
                        rhs: new_rhs,
                        op,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::Unary { expr: inner, op } => {
                let new_inner = self.capture_expr(module, inner)?;

                if unchanged(inner, new_inner) {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::Unary {
                        expr: new_inner,
                        op,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::Call { callee, args } => {
                let new_callee = self.capture_expr(module, callee)?;
                let (new_args, args_changed) = self.capture_expr_list(module, args)?;

                if unchanged(callee, new_callee) && !args_changed {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::Call {
                        callee: new_callee,
                        args: new_args,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::MacroCall { name, args } => {
                if self.is_capture_macro(name.0) {
                    return Ok(expr);
                }

                let (new_args, vars_changed) = self.capture_expr_list(module, args)?;

                if !vars_changed {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::MacroCall {
                        name,
                        args: new_args,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let new_condition = self.capture_expr(module, condition)?;
                let new_then = self.capture_statement(module, then_block)?;
                let new_else = match else_block {
                    Some(block) => Some(self.capture_statement(module, block)?),
                    None => None,
                };

                let changed = !unchanged(condition, new_condition)
                    || !unchanged(then_block, new_then)
                    || !option_unchanged(else_block, new_else);

                if !changed {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::If {
                        condition: new_condition,
                        then_block: new_then,
                        else_block: new_else,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::Switch { object, arms } => {
                let new_object = self.capture_expr(module, object)?;
                let (new_arms, arms_changed) = self.capture_arm_list(module, arms)?;

                if unchanged(object, new_object) && !arms_changed {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::Switch {
                        object: new_object,
                        arms: new_arms,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::FieldAccess { object, field } => {
                let new_object = self.capture_expr(module, object)?;
                let new_field = self.capture_expr(module, field)?;

                if unchanged(object, new_object) && unchanged(field, new_field) {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::FieldAccess {
                        object: new_object,
                        field: new_field,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::SliceAccess { object, index } => {
                let new_object = self.capture_expr(module, object)?;
                let new_index = self.capture_expr(module, index)?;

                if unchanged(object, new_object) && unchanged(index, new_index) {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::SliceAccess {
                        object: new_object,
                        index: new_index,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::StructInit { ty, fields } => {
                let new_ty = self.capture_expr(module, ty)?;
                let (new_fields, fields_changed): (Option<&'arena [FieldInit<'arena>]>, bool) =
                    match fields {
                        Some(fields) => {
                            let mut rebuilt: Vec<FieldInit<'arena>> =
                                Vec::with_capacity(fields.len());
                            let mut changed = false;

                            for field in fields.iter() {
                                let value = self.capture_expr(module, field.value)?;

                                if unchanged(field.value, value) {
                                    rebuilt.push(*field);
                                } else {
                                    changed = true;
                                    rebuilt.push(FieldInit {
                                        name: field.name,
                                        value,
                                        span: field.span,
                                    });
                                }
                            }

                            (Some(self.arena.alloc_slice_copy(&rebuilt)), changed)
                        }
                        None => (None, false),
                    };

                if unchanged(ty, new_ty) && !fields_changed {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::StructInit {
                        ty: new_ty,
                        fields: new_fields,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::ArrayInit { elements } => {
                let (new_elements, elements_changed) = self.capture_expr_list(module, elements)?;

                if !elements_changed {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::ArrayInit {
                        elements: new_elements,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::ArrayRepeatInit { element, len } => {
                let new_element = self.capture_expr(module, element)?;
                let new_len = self.capture_expr(module, len)?;

                if unchanged(element, new_element) && unchanged(len, new_len) {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::ArrayRepeatInit {
                        element: new_element,
                        len: new_len,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::Block { stmts, trailing } => {
                let (new_stmts, stmts_changed) = self.capture_statement_list(module, stmts)?;
                let new_trailing = match trailing {
                    Some(trailing_expr) => Some(self.capture_expr(module, trailing_expr)?),
                    None => None,
                };

                if !stmts_changed && option_unchanged(trailing, new_trailing) {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::Block {
                        stmts: new_stmts,
                        trailing: new_trailing,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::Closure {
                params,
                return_type,
                body,
            } => {
                let new_params = self.capture_params(module, params)?;
                let new_return = match return_type {
                    Some(ty) => Some(self.capture_type(module, ty)?),
                    None => None,
                };
                let new_body = self.capture_statement(module, body)?;

                if unchanged(params, new_params)
                    && option_unchanged(return_type, new_return)
                    && unchanged(body, new_body)
                {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::Closure {
                        params: new_params,
                        return_type: new_return,
                        body: new_body,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::Type(ty) => {
                let new_ty = self.capture_type(module, ty)?;

                if unchanged(ty, new_ty) {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::Type(new_ty),
                    span: expr.span,
                }))
            }

            ExpressionKind::Decl(decl) => {
                let new_decl = self.capture_decl(module, decl)?;

                if unchanged(decl, new_decl) {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::Decl(new_decl),
                    span: expr.span,
                }))
            }

            ExpressionKind::Range {
                start,
                end,
                inclusive,
            } => {
                let new_start = match start {
                    Some(start_expr) => Some(self.capture_expr(module, start_expr)?),
                    None => None,
                };
                let new_end = match end {
                    Some(end_expr) => Some(self.capture_expr(module, end_expr)?),
                    None => None,
                };

                if option_unchanged(start, new_start) && option_unchanged(end, new_end) {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::Range {
                        start: new_start,
                        end: new_end,
                        inclusive,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::ComptimeBlock { stmts, trailing } => {
                let (new_stmts, stmts_changed) = self.capture_statement_list(module, stmts)?;
                let new_trailing = match trailing {
                    Some(trailing_expr) => Some(self.capture_expr(module, trailing_expr)?),
                    None => None,
                };

                if !stmts_changed && option_unchanged(trailing, new_trailing) {
                    return Ok(expr);
                }

                Ok(self.arena.alloc(Expression {
                    kind: ExpressionKind::ComptimeBlock {
                        stmts: new_stmts,
                        trailing: new_trailing,
                    },
                    span: expr.span,
                }))
            }

            ExpressionKind::ConditionalBlock(_) => {
                unreachable!("conditional expressions are expanded before comptime")
            }

            ExpressionKind::Splice { .. } => unreachable!("splices are expanded above"),
        }
    }

    pub(crate) fn capture_type(
        &mut self,
        module: &PathBuf,
        ty: &'arena TypeExpr<'arena>,
    ) -> Result<&'arena TypeExpr<'arena>, ()> {
        if let TypeKind::Splice(inner) = &ty.kind {
            return self.splice_type(module, inner, ty.span);
        }

        match ty.kind {
            TypeKind::Builtin(_) | TypeKind::SelfType | TypeKind::SelfAlias | TypeKind::VaArgs => {
                Ok(ty)
            }

            TypeKind::Named { name, generic_args } => {
                let Some(args) = generic_args else {
                    return Ok(ty);
                };

                let (new_args, changed) = self.capture_type_list(module, args)?;

                if !changed {
                    return Ok(ty);
                }

                Ok(self.arena.alloc(TypeExpr {
                    kind: TypeKind::Named {
                        name,
                        generic_args: Some(new_args),
                    },
                    span: ty.span,
                }))
            }

            TypeKind::Const(inner) => {
                let new_inner = self.capture_type(module, inner)?;

                if unchanged(inner, new_inner) {
                    return Ok(ty);
                }

                Ok(self.arena.alloc(TypeExpr {
                    kind: TypeKind::Const(new_inner),
                    span: ty.span,
                }))
            }

            TypeKind::TypeOf(expr) => {
                let new_expr = self.capture_expr(module, expr)?;

                if unchanged(expr, new_expr) {
                    return Ok(ty);
                }

                Ok(self.arena.alloc(TypeExpr {
                    kind: TypeKind::TypeOf(new_expr),
                    span: ty.span,
                }))
            }

            TypeKind::ComptimeCall(expr) => {
                let new_expr = self.capture_expr(module, expr)?;

                if unchanged(expr, new_expr) {
                    return Ok(ty);
                }

                Ok(self.arena.alloc(TypeExpr {
                    kind: TypeKind::ComptimeCall(new_expr),
                    span: ty.span,
                }))
            }

            TypeKind::SinglePointer(inner) | TypeKind::ManyPointer(inner) => {
                let new_inner = self.capture_type(module, inner)?;

                if unchanged(inner, new_inner) {
                    return Ok(ty);
                }

                let kind = match ty.kind {
                    TypeKind::SinglePointer(_) => TypeKind::SinglePointer(new_inner),
                    _ => TypeKind::ManyPointer(new_inner),
                };

                Ok(self.arena.alloc(TypeExpr {
                    kind,
                    span: ty.span,
                }))
            }

            TypeKind::Array { element, len } => {
                let new_element = self.capture_type(module, element)?;
                let new_len = match len {
                    Some(len_expr) => Some(self.capture_expr(module, len_expr)?),
                    None => None,
                };

                if unchanged(element, new_element) && option_unchanged(len, new_len) {
                    return Ok(ty);
                }

                Ok(self.arena.alloc(TypeExpr {
                    kind: TypeKind::Array {
                        element: new_element,
                        len: new_len,
                    },
                    span: ty.span,
                }))
            }

            TypeKind::Fn {
                params,
                generic_args,
                ret,
            } => {
                let (new_params, params_changed) = self.capture_type_list(module, params)?;
                let new_ret = self.capture_type(module, ret)?;

                if !params_changed && unchanged(ret, new_ret) {
                    return Ok(ty);
                }

                Ok(self.arena.alloc(TypeExpr {
                    kind: TypeKind::Fn {
                        params: new_params,
                        generic_args,
                        ret: new_ret,
                    },
                    span: ty.span,
                }))
            }

            TypeKind::FatFn { params, ret, once } => {
                let (new_params, params_changed) = self.capture_type_list(module, params)?;
                let new_ret = self.capture_type(module, ret)?;

                if !params_changed && unchanged(ret, new_ret) {
                    return Ok(ty);
                }

                Ok(self.arena.alloc(TypeExpr {
                    kind: TypeKind::FatFn {
                        params: new_params,
                        ret: new_ret,
                        once,
                    },
                    span: ty.span,
                }))
            }

            TypeKind::Splice(_) => unreachable!("splices are expanded above"),
        }
    }

    pub(crate) fn capture_decl(
        &mut self,
        module: &PathBuf,
        decl: &'arena Declaration<'arena>,
    ) -> Result<&'arena Declaration<'arena>, ()> {
        match decl.kind {
            DeclarationKind::FnDecl {
                name,
                generics,
                params,
                return_type,
                body,
                is_pub,
                is_extern,
            } => {
                let (name, name_changed) = self.capture_decl_name(module, name)?;
                let new_params = self.capture_params(module, params)?;
                let new_return = match return_type {
                    Some(ty) => Some(self.capture_type(module, ty)?),
                    None => None,
                };
                let new_body = match body {
                    Some(body) => Some(self.capture_statement(module, body)?),
                    None => None,
                };

                if !name_changed
                    && unchanged(params, new_params)
                    && option_unchanged(return_type, new_return)
                    && option_unchanged(body, new_body)
                {
                    return Ok(decl);
                }

                Ok(self.arena.alloc(Declaration {
                    kind: DeclarationKind::FnDecl {
                        name,
                        generics,
                        params: new_params,
                        return_type: new_return,
                        body: new_body,
                        is_pub,
                        is_extern,
                    },
                    source: decl.source.clone(),
                }))
            }

            DeclarationKind::StructDecl {
                name,
                is_pub,
                generics,
                fields,
                methods,
            } => {
                let (name, name_changed) = self.capture_decl_name(module, name)?;
                let (new_fields, fields_changed) = self.capture_fields(module, fields)?;
                let (new_methods, methods_changed) = self.capture_decl_list(module, methods)?;

                if !name_changed && !fields_changed && !methods_changed {
                    return Ok(decl);
                }

                Ok(self.arena.alloc(Declaration {
                    kind: DeclarationKind::StructDecl {
                        name,
                        is_pub,
                        generics,
                        fields: new_fields,
                        methods: new_methods,
                    },
                    source: decl.source.clone(),
                }))
            }

            DeclarationKind::EnumDecl {
                name,
                is_pub,
                generics,
                variants,
                methods,
            } => {
                let (name, name_changed) = self.capture_decl_name(module, name)?;
                let mut new_variants = Vec::with_capacity(variants.len());
                let mut variants_changed = false;

                for variant in variants.iter() {
                    let payload = match &variant.payload {
                        Some(EnumVariantPayload::Single(ty)) => {
                            Some(EnumVariantPayload::Single(self.capture_type(module, ty)?))
                        }
                        Some(EnumVariantPayload::Anonymous(fields)) => {
                            let (new_fields, _) = self.capture_fields(module, fields)?;
                            Some(EnumVariantPayload::Anonymous(new_fields))
                        }
                        None => None,
                    };

                    let payload_changed = match (&variant.payload, &payload) {
                        (
                            Some(EnumVariantPayload::Single(a)),
                            Some(EnumVariantPayload::Single(b)),
                        ) => !std::ptr::eq(*a, *b),
                        (
                            Some(EnumVariantPayload::Anonymous(a)),
                            Some(EnumVariantPayload::Anonymous(b)),
                        ) => !std::ptr::eq(*a, *b),
                        (None, None) => false,
                        _ => true,
                    };

                    if payload_changed {
                        variants_changed = true;
                    }

                    new_variants.push(zeen_ast::declarations::EnumVariant {
                        name: variant.name,
                        span: variant.span,
                        payload,
                    });
                }

                let (new_methods, methods_changed) = self.capture_decl_list(module, methods)?;

                if !name_changed && !variants_changed && !methods_changed {
                    return Ok(decl);
                }

                Ok(self.arena.alloc(Declaration {
                    kind: DeclarationKind::EnumDecl {
                        name,
                        is_pub,
                        generics,
                        variants: self.arena.alloc_slice_copy(&new_variants),
                        methods: new_methods,
                    },
                    source: decl.source.clone(),
                }))
            }

            DeclarationKind::ComptimeStruct {
                name,
                is_pub,
                fields,
                methods,
            } => {
                let (new_fields, fields_changed) = self.capture_fields(module, fields)?;
                let (new_methods, methods_changed) = self.capture_decl_list(module, methods)?;

                if !fields_changed && !methods_changed {
                    return Ok(decl);
                }

                Ok(self.arena.alloc(Declaration {
                    kind: DeclarationKind::ComptimeStruct {
                        name,
                        is_pub,
                        fields: new_fields,
                        methods: new_methods,
                    },
                    source: decl.source.clone(),
                }))
            }

            DeclarationKind::ComptimeFn {
                name,
                params,
                return_type,
                body,
                is_pub,
            } => {
                let new_params = self.capture_params(module, params)?;
                let new_return = match return_type {
                    Some(ty) => Some(self.capture_type(module, ty)?),
                    None => None,
                };
                let new_body = match body {
                    Some(body) => Some(self.capture_statement(module, body)?),
                    None => None,
                };

                if unchanged(params, new_params)
                    && option_unchanged(return_type, new_return)
                    && option_unchanged(body, new_body)
                {
                    return Ok(decl);
                }

                Ok(self.arena.alloc(Declaration {
                    kind: DeclarationKind::ComptimeFn {
                        name,
                        params: new_params,
                        return_type: new_return,
                        body: new_body,
                        is_pub,
                    },
                    source: decl.source.clone(),
                }))
            }

            DeclarationKind::ComptimeConst {
                name,
                value,
                is_pub,
            } => {
                let new_value = self.capture_expr(module, value)?;

                if unchanged(value, new_value) {
                    return Ok(decl);
                }

                Ok(self.arena.alloc(Declaration {
                    kind: DeclarationKind::ComptimeConst {
                        name,
                        value: new_value,
                        is_pub,
                    },
                    source: decl.source.clone(),
                }))
            }

            DeclarationKind::ComptimeBlock(block) => {
                let (new_stmts, stmts_changed) =
                    self.capture_statement_list(module, block.stmts)?;

                if !stmts_changed {
                    return Ok(decl);
                }

                Ok(self.arena.alloc(Declaration {
                    kind: DeclarationKind::ComptimeBlock(
                        self.arena
                            .alloc(zeen_ast::declarations::ComptimeBlock { stmts: new_stmts }),
                    ),
                    source: decl.source.clone(),
                }))
            }

            DeclarationKind::GlobalVar {
                name,
                ty,
                value,
                is_const,
                is_pub,
            } => {
                let (name, name_changed) = self.capture_decl_name(module, name)?;
                let new_ty = self.capture_type(module, ty)?;
                let new_value = self.capture_expr(module, value)?;

                if !name_changed && unchanged(ty, new_ty) && unchanged(value, new_value) {
                    return Ok(decl);
                }

                Ok(self.arena.alloc(Declaration {
                    kind: DeclarationKind::GlobalVar {
                        name,
                        ty: new_ty,
                        value: new_value,
                        is_const,
                        is_pub,
                    },
                    source: decl.source.clone(),
                }))
            }

            DeclarationKind::ExternVar { name, ty, is_pub } => {
                let (name, name_changed) = self.capture_decl_name(module, name)?;
                let new_ty = self.capture_type(module, ty)?;

                if !name_changed && unchanged(ty, new_ty) {
                    return Ok(decl);
                }

                Ok(self.arena.alloc(Declaration {
                    kind: DeclarationKind::ExternVar {
                        name,
                        ty: new_ty,
                        is_pub,
                    },
                    source: decl.source.clone(),
                }))
            }

            DeclarationKind::Alias(alias) => {
                let (name, name_changed) = self.capture_decl_name(module, alias.name)?;
                let new_ty = self.capture_type(module, alias.ty)?;

                if !name_changed && unchanged(alias.ty, new_ty) {
                    return Ok(decl);
                }

                let rebuilt = zeen_ast::declarations::AliasDecl {
                    name,
                    is_pub: alias.is_pub,
                    generics: alias.generics,
                    ty: new_ty,
                };

                Ok(self.arena.alloc(Declaration {
                    kind: DeclarationKind::Alias(rebuilt),
                    source: decl.source.clone(),
                }))
            }

            DeclarationKind::InterfaceDecl {
                name,
                is_pub,
                generics,
                methods,
            } => {
                let (name, name_changed) = self.capture_decl_name(module, name)?;
                let (new_methods, methods_changed) = self.capture_decl_list(module, methods)?;

                if !name_changed && !methods_changed {
                    return Ok(decl);
                }

                Ok(self.arena.alloc(Declaration {
                    kind: DeclarationKind::InterfaceDecl {
                        name,
                        is_pub,
                        generics,
                        methods: new_methods,
                    },
                    source: decl.source.clone(),
                }))
            }

            DeclarationKind::ImplementDecl {
                interface,
                builtin,
                object,
                generics,
                methods,
            } => {
                let (new_methods, methods_changed) = self.capture_decl_list(module, methods)?;

                if !methods_changed {
                    return Ok(decl);
                }

                Ok(self.arena.alloc(Declaration {
                    kind: DeclarationKind::ImplementDecl {
                        interface,
                        builtin,
                        object,
                        generics,
                        methods: new_methods,
                    },
                    source: decl.source.clone(),
                }))
            }

            DeclarationKind::Use { .. }
            | DeclarationKind::ExternLink { .. }
            | DeclarationKind::ExternInclude { .. } => Ok(decl),

            DeclarationKind::ConditionalBlock(_) => {
                unreachable!("conditional declarations are expanded before comptime")
            }
        }
    }

    fn capture_decl_name(
        &mut self,
        module: &PathBuf,
        name: (Spur, miette::SourceSpan),
    ) -> Result<((Spur, miette::SourceSpan), bool), ()> {
        let text = self.interned(&name.0);

        let Some(inner) = text
            .strip_prefix("$(")
            .and_then(|value| value.strip_suffix(')'))
        else {
            return Ok((name, false));
        };

        let fragment = inner.to_owned();
        let value = match self.eval_interpolation_fragment(module, &fragment, name.1) {
            Ok(value) => value,
            Err(_) => return Err(()),
        };

        match value {
            Value::Str(value) => {
                let resolved = self.interner.borrow_mut().get_or_intern(value.as_ref());

                Ok(((resolved, name.1), true))
            }
            other => {
                self.errors.push(Error::TypeMismatch {
                    message: format!(
                        "a declaration name splice expects a comptime_str, got {}",
                        other.type_name()
                    )
                    .into(),
                    src: self.current_src(),
                    span: name.1,
                });

                Err(())
            }
        }
    }

    fn splice_type(
        &mut self,
        module: &PathBuf,
        inner: &'arena Expression<'arena>,
        span: miette::SourceSpan,
    ) -> Result<&'arena TypeExpr<'arena>, ()> {
        if self.capture_depth >= CAPTURE_DEPTH_LIMIT {
            self.errors.push(Error::LimitExceeded {
                message: format!(
                    "comptime capture nesting exceeded the limit of {CAPTURE_DEPTH_LIMIT}"
                )
                .into(),
                src: self.current_src(),
                span,
            });
            return Err(());
        }

        let value = match self.eval_expr(inner) {
            Ok(value) => value,
            Err(_) => return Err(()),
        };

        match value {
            Value::Type(ty) => {
                self.capture_depth += 1;
                let result = self.capture_type(module, ty);
                self.capture_depth -= 1;
                result
            }
            other => {
                self.errors.push(Error::TypeMismatch {
                    message: format!(
                        "`$()` in a type position expects a zeen_type, got {}",
                        other.type_name()
                    )
                    .into(),
                    src: self.current_src(),
                    span,
                });
                Err(())
            }
        }
    }

    fn splice_expr(
        &mut self,
        module: &PathBuf,
        inner: &'arena Expression<'arena>,
        span: miette::SourceSpan,
    ) -> Result<&'arena Expression<'arena>, ()> {
        if self.capture_depth >= CAPTURE_DEPTH_LIMIT {
            self.errors.push(Error::LimitExceeded {
                message: format!(
                    "comptime capture nesting exceeded the limit of {CAPTURE_DEPTH_LIMIT}"
                )
                .into(),
                src: self.current_src(),
                span,
            });
            return Err(());
        }

        let value = match self.eval_expr(inner) {
            Ok(value) => value,
            Err(_) => return Err(()),
        };

        match value {
            Value::Expr(captured) => {
                self.capture_depth += 1;
                let result = self.capture_expr(module, captured);
                self.capture_depth -= 1;
                result
            }
            Value::Decl(_) => {
                self.errors.push(Error::TypeMismatch {
                    message: "`$()` cannot insert a zeen_decl value".into(),
                    src: self.current_src(),
                    span,
                });
                Err(())
            }
            other => match self.value_into_expr(other, span) {
                Ok(expression) => Ok(self.arena.alloc(expression)),
                Err(error) => {
                    self.report(error);
                    Err(())
                }
            },
        }
    }

    fn capture_statement(
        &mut self,
        module: &PathBuf,
        stmt: &'arena Statement<'arena>,
    ) -> Result<&'arena Statement<'arena>, ()> {
        match stmt.kind {
            StatementKind::Let {
                name,
                explicit_type,
                value,
                is_const,
            } => {
                let new_type = match explicit_type {
                    Some(ty) => Some(self.capture_type(module, ty)?),
                    None => None,
                };
                let new_value = match value {
                    Some(value_expr) => Some(self.capture_expr(module, value_expr)?),
                    None => None,
                };

                if option_unchanged(explicit_type, new_type) && option_unchanged(value, new_value) {
                    return Ok(stmt);
                }

                Ok(self.arena.alloc(Statement {
                    kind: StatementKind::Let {
                        name,
                        explicit_type: new_type,
                        value: new_value,
                        is_const,
                    },
                    span: stmt.span,
                }))
            }

            StatementKind::Assign { object, value } => {
                let new_object = self.capture_expr(module, object)?;
                let new_value = self.capture_expr(module, value)?;

                if unchanged(object, new_object) && unchanged(value, new_value) {
                    return Ok(stmt);
                }

                Ok(self.arena.alloc(Statement {
                    kind: StatementKind::Assign {
                        object: new_object,
                        value: new_value,
                    },
                    span: stmt.span,
                }))
            }

            StatementKind::CompoundAssign { object, value, op } => {
                let new_object = self.capture_expr(module, object)?;
                let new_value = self.capture_expr(module, value)?;

                if unchanged(object, new_object) && unchanged(value, new_value) {
                    return Ok(stmt);
                }

                Ok(self.arena.alloc(Statement {
                    kind: StatementKind::CompoundAssign {
                        object: new_object,
                        value: new_value,
                        op,
                    },
                    span: stmt.span,
                }))
            }

            StatementKind::Return { value } => {
                let new_value = match value {
                    Some(value_expr) => Some(self.capture_expr(module, value_expr)?),
                    None => None,
                };

                if option_unchanged(value, new_value) {
                    return Ok(stmt);
                }

                Ok(self.arena.alloc(Statement {
                    kind: StatementKind::Return { value: new_value },
                    span: stmt.span,
                }))
            }

            StatementKind::While { condition, block } => {
                let new_condition = self.capture_expr(module, condition)?;
                let new_block = self.capture_statement(module, block)?;

                if unchanged(condition, new_condition) && unchanged(block, new_block) {
                    return Ok(stmt);
                }

                Ok(self.arena.alloc(Statement {
                    kind: StatementKind::While {
                        condition: new_condition,
                        block: new_block,
                    },
                    span: stmt.span,
                }))
            }

            StatementKind::For {
                varname,
                iterator,
                block,
            } => {
                let new_iterator = self.capture_expr(module, iterator)?;
                let new_block = self.capture_statement(module, block)?;

                if unchanged(iterator, new_iterator) && unchanged(block, new_block) {
                    return Ok(stmt);
                }

                Ok(self.arena.alloc(Statement {
                    kind: StatementKind::For {
                        varname,
                        iterator: new_iterator,
                        block: new_block,
                    },
                    span: stmt.span,
                }))
            }

            StatementKind::Expr(expr) => {
                let new_expr = self.capture_expr(module, expr)?;

                if unchanged(expr, new_expr) {
                    return Ok(stmt);
                }

                Ok(self.arena.alloc(Statement {
                    kind: StatementKind::Expr(new_expr),
                    span: stmt.span,
                }))
            }

            StatementKind::TrailingExpr(expr) => {
                let new_expr = self.capture_expr(module, expr)?;

                if unchanged(expr, new_expr) {
                    return Ok(stmt);
                }

                Ok(self.arena.alloc(Statement {
                    kind: StatementKind::TrailingExpr(new_expr),
                    span: stmt.span,
                }))
            }

            StatementKind::FnDecl(decl) => {
                let new_decl = self.capture_decl(module, decl)?;

                if unchanged(decl, new_decl) {
                    return Ok(stmt);
                }

                Ok(self.arena.alloc(Statement {
                    kind: StatementKind::FnDecl(new_decl),
                    span: stmt.span,
                }))
            }

            StatementKind::Break | StatementKind::Continue => Ok(stmt),

            StatementKind::ConditionalBlock(_) => {
                unreachable!("conditional statements are expanded before comptime")
            }
        }
    }

    fn capture_statement_list(
        &mut self,
        module: &PathBuf,
        stmts: &'arena [&'arena Statement<'arena>],
    ) -> Result<(&'arena [&'arena Statement<'arena>], bool), ()> {
        let mut rebuilt: Vec<&'arena Statement<'arena>> = Vec::with_capacity(stmts.len());
        let mut changed = false;

        for stmt in stmts.iter() {
            let new_stmt = self.capture_statement(module, stmt)?;

            if unchanged(*stmt, new_stmt) {
                rebuilt.push(stmt);
            } else {
                changed = true;
                rebuilt.push(new_stmt);
            }
        }

        if !changed {
            return Ok((stmts, false));
        }

        Ok((self.arena.alloc_slice_copy(&rebuilt), true))
    }

    fn capture_expr_list(
        &mut self,
        module: &PathBuf,
        exprs: &'arena [&'arena Expression<'arena>],
    ) -> Result<(&'arena [&'arena Expression<'arena>], bool), ()> {
        let mut rebuilt: Vec<&'arena Expression<'arena>> = Vec::with_capacity(exprs.len());
        let mut changed = false;

        for expr in exprs.iter() {
            let new_expr = self.capture_expr(module, expr)?;

            if unchanged(*expr, new_expr) {
                rebuilt.push(expr);
            } else {
                changed = true;
                rebuilt.push(new_expr);
            }
        }

        if !changed {
            return Ok((exprs, false));
        }

        Ok((self.arena.alloc_slice_copy(&rebuilt), true))
    }

    fn capture_type_list(
        &mut self,
        module: &PathBuf,
        types: &'arena [&'arena TypeExpr<'arena>],
    ) -> Result<(&'arena [&'arena TypeExpr<'arena>], bool), ()> {
        let mut rebuilt: Vec<&'arena TypeExpr<'arena>> = Vec::with_capacity(types.len());
        let mut changed = false;

        for ty in types.iter() {
            let new_ty = self.capture_type(module, ty)?;

            if unchanged(*ty, new_ty) {
                rebuilt.push(ty);
            } else {
                changed = true;
                rebuilt.push(new_ty);
            }
        }

        if !changed {
            return Ok((types, false));
        }

        Ok((self.arena.alloc_slice_copy(&rebuilt), true))
    }

    fn capture_params(
        &mut self,
        module: &PathBuf,
        params: &'arena [FnParam<'arena>],
    ) -> Result<&'arena [FnParam<'arena>], ()> {
        let mut rebuilt: Vec<FnParam<'arena>> = Vec::with_capacity(params.len());
        let mut changed = false;

        for param in params.iter() {
            let new_ty = self.capture_type(module, param.ty)?;

            if unchanged(param.ty, new_ty) {
                rebuilt.push(*param);
            } else {
                changed = true;
                rebuilt.push(FnParam {
                    name: param.name,
                    ty: new_ty,
                    span: param.span,
                });
            }
        }

        if !changed {
            return Ok(params);
        }

        Ok(self.arena.alloc_slice_copy(&rebuilt))
    }

    fn capture_fields(
        &mut self,
        module: &PathBuf,
        fields: &'arena [StructField<'arena>],
    ) -> Result<(&'arena [StructField<'arena>], bool), ()> {
        let mut rebuilt: Vec<StructField<'arena>> = Vec::with_capacity(fields.len());
        let mut changed = false;

        for field in fields.iter() {
            let new_ty = self.capture_type(module, field.ty)?;

            if unchanged(field.ty, new_ty) {
                rebuilt.push(*field);
            } else {
                changed = true;
                rebuilt.push(StructField {
                    name: field.name,
                    ty: new_ty,
                    is_pub: field.is_pub,
                });
            }
        }

        if !changed {
            return Ok((fields, false));
        }

        Ok((self.arena.alloc_slice_copy(&rebuilt), true))
    }

    fn capture_decl_list(
        &mut self,
        module: &PathBuf,
        decls: &'arena [&'arena Declaration<'arena>],
    ) -> Result<(&'arena [&'arena Declaration<'arena>], bool), ()> {
        let mut rebuilt: Vec<&'arena Declaration<'arena>> = Vec::with_capacity(decls.len());
        let mut changed = false;

        for decl in decls.iter() {
            let new_decl = self.capture_decl(module, decl)?;

            if unchanged(*decl, new_decl) {
                rebuilt.push(decl);
            } else {
                changed = true;
                rebuilt.push(new_decl);
            }
        }

        if !changed {
            return Ok((decls, false));
        }

        Ok((self.arena.alloc_slice_copy(&rebuilt), true))
    }

    fn capture_arm_list(
        &mut self,
        module: &PathBuf,
        arms: &'arena [&'arena Arm<'arena>],
    ) -> Result<(&'arena [&'arena Arm<'arena>], bool), ()> {
        let mut rebuilt: Vec<&'arena Arm<'arena>> = Vec::with_capacity(arms.len());
        let mut changed = false;

        for arm in arms.iter() {
            let new_body = self.capture_expr(module, arm.body)?;
            let new_guard = match arm.guard {
                Some(guard) => Some(self.capture_expr(module, guard)?),
                None => None,
            };

            let body_changed = !unchanged(arm.body, new_body);
            let guard_changed = !option_unchanged(arm.guard, new_guard);

            if !body_changed && !guard_changed {
                rebuilt.push(arm);
            } else {
                changed = true;
                rebuilt.push(self.arena.alloc(Arm {
                    pattern: arm.pattern,
                    body: new_body,
                    guard: new_guard,
                }));
            }
        }

        if !changed {
            return Ok((arms, false));
        }

        Ok((self.arena.alloc_slice_copy(&rebuilt), true))
    }
}

fn unchanged<T: ?Sized>(original: &T, rebuilt: &T) -> bool {
    std::ptr::eq(original, rebuilt)
}

fn option_unchanged<T>(original: Option<&T>, rebuilt: Option<&T>) -> bool {
    match (original, rebuilt) {
        (None, None) => true,
        (Some(a), Some(b)) => std::ptr::eq(a, b),
        _ => false,
    }
}
