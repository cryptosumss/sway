use sway_error::{
    error::CompileError,
    handler::{ErrorEmitted, Handler},
};
use sway_types::{Ident, Span, Spanned};

use crate::{
    decl_engine::DeclRefStruct,
    language::{parsed::*, ty, CallPath, Visibility},
    semantic_analysis::{
        type_check_context::EnforceTypeArguments, GenericShadowingMode, TypeCheckContext,
    },
    type_system::*,
};

const UNIFY_STRUCT_FIELD_HELP_TEXT: &str =
    "Struct field's type must match the type specified in its declaration.";

pub(crate) fn struct_instantiation(
    handler: &Handler,
    mut ctx: TypeCheckContext,
    mut call_path_binding: TypeBinding<CallPath>,
    fields: Vec<StructExpressionField>,
    span: Span,
) -> Result<ty::TyExpression, ErrorEmitted> {
    let type_engine = ctx.engines.te();
    let decl_engine = ctx.engines.de();
    let engines = ctx.engines();

    // We need the call_path_binding to have types that point to proper definitions so the LSP can
    // look for them, but its types haven't been resolved yet.
    // To that end we do a dummy type check which has the side effect of resolving the types.
    let _: Result<(DeclRefStruct, _, _), _> =
        TypeBinding::type_check(&mut call_path_binding, &Handler::default(), ctx.by_ref());

    let TypeBinding {
        inner: CallPath {
            prefixes, suffix, ..
        },
        type_arguments,
        span: inner_span,
    } = call_path_binding.clone();

    if let TypeArgs::Prefix(_) = type_arguments {
        return Err(
            handler.emit_err(CompileError::DoesNotTakeTypeArgumentsAsPrefix {
                name: suffix,
                span: type_arguments.span(),
            }),
        );
    }

    let type_arguments = type_arguments.to_vec();

    let type_info = match (suffix.as_str(), type_arguments.is_empty()) {
        ("Self", true) => TypeInfo::new_self_type(suffix.span()),
        ("Self", false) => {
            return Err(handler.emit_err(CompileError::TypeArgumentsNotAllowed {
                span: suffix.span(),
            }));
        }
        (_, true) => TypeInfo::Custom {
            qualified_call_path: suffix.clone().into(),
            type_arguments: None,
            root_type_id: None,
        },
        (_, false) => TypeInfo::Custom {
            qualified_call_path: suffix.clone().into(),
            type_arguments: Some(type_arguments),
            root_type_id: None,
        },
    };

    // find the module that the struct decl is in
    let type_info_prefix = ctx.namespace.find_module_path(&prefixes);
    ctx.namespace
        .root()
        .check_submodule(handler, &type_info_prefix)?;

    // resolve the type of the struct decl
    let type_id = ctx
        .resolve_type(
            handler,
            type_engine.insert(engines, type_info, suffix.span().source_id()),
            &inner_span,
            EnforceTypeArguments::No,
            Some(&type_info_prefix),
        )
        .unwrap_or_else(|err| type_engine.insert(engines, TypeInfo::ErrorRecovery(err), None));

    // extract the struct name and fields from the type info
    let type_info = type_engine.get(type_id);
    let struct_ref = type_info.expect_struct(handler, engines, &span)?;
    let struct_decl = (*decl_engine.get_struct(&struct_ref)).clone();
    let struct_name = struct_decl.call_path.suffix;
    let struct_fields = struct_decl.fields;
    let mut struct_fields = struct_fields;

    // To avoid conflicting and overlapping errors, we follow the Rust approach:
    // - Missing fields are reported only if the struct can actually be instantiated.
    // - Individual fields issues are always reported: private field access, non-existing fields.

    assert!(struct_decl.call_path.is_absolute, "The call path of the struct field declaration must always be absolute.");

    // For solution suggestions, we assume that the programmers can adapt the struct (e.g., changing field privacy)
    // if the struct is in the same package where the issue is.
    // A bit too restrictive, considering the same workspace might be more appropriate, but it will work for now.
    let struct_can_be_adapted = !ctx.namespace.module_is_external(&struct_decl.call_path.prefixes);

    let is_out_of_decl_module_instantiation = !ctx.namespace.module_is_submodule_of(&struct_decl.call_path.prefixes, true);
    let struct_has_private_fields = struct_fields.iter().any(|field| matches!(field.visibility, Visibility::Private));
    let struct_can_be_instantiated = !is_out_of_decl_module_instantiation || !struct_has_private_fields;
    
    let typed_fields = type_check_field_arguments(
        handler,
        ctx.by_ref(),
        &fields,
        &struct_name,
        &mut struct_fields,
        &span,
        &struct_decl.span,
        // Emit the missing fields error only if the struct can actually be instantiated.
        struct_can_be_instantiated
    )?;

    if !struct_can_be_instantiated {
        handler.emit_err(CompileError::StructCannotBeInstantiated {
            struct_name: struct_name.clone(),
            span: span.clone(),
            struct_decl_span: struct_decl.span.clone(),
            private_fields: struct_fields.iter().filter(|field| matches!(field.visibility, Visibility::Private)).map(|field| field.name.clone()).collect(),
            all_fields_are_private: struct_fields.iter().all(|field| matches!(field.visibility, Visibility::Private)),
            struct_can_be_adapted,
        });
    }

    unify_field_arguments_and_struct_fields(handler, ctx.by_ref(), &typed_fields, &struct_fields)?;

    // Unify type id with type annotation so eventual generic type parameters are properly resolved.
    // When a generic type parameter is not used in field arguments it should be unified with type annotation.
    type_engine.unify(
        handler,
        engines,
        type_id,
        ctx.type_annotation(),
        &span,
        "Struct type must match the type specified in its declaration.",
        None,
    );

    // Check that there are no extra fields.
    for field in fields.iter() {
        if !struct_fields.iter().any(|x| x.name == field.name) {
            handler.emit_err(CompileError::StructDoesNotHaveField {
                field_name: field.name.clone(),
                struct_name: struct_name.clone(),
                span: field.name.span(),
            });
        }
    }

    // If the current module being checked is not a submodule of the
    // module in which the struct is declared, check for private fields usage.
    if is_out_of_decl_module_instantiation {
        for field in fields {
            if let Some(ty_field) = struct_fields.iter().find(|x| x.name == field.name) {
                if matches!(ty_field.visibility, Visibility::Private) {
                    handler.emit_err(CompileError::StructFieldIsPrivate {
                        field_name: field.name.clone(),
                        struct_name: struct_name.clone(),
                        span: field.name.span(),
                        field_decl_span: ty_field.name.span(),
                        // Suppress struct changing suggestions, because we already gave them in the
                        // StructCannotBeInstantiated error.
                        struct_can_be_adapted: false,
                    });
                }
            }
        }
    }

    let mut struct_namespace = ctx.namespace.clone();
    let mut struct_ctx = ctx
        .scoped(&mut struct_namespace)
        .with_generic_shadowing_mode(GenericShadowingMode::Allow);

    // Insert struct type parameter into namespace.
    // This is required so check_type_parameter_bounds can resolve generic trait type parameters.
    for type_parameter in struct_decl.type_parameters {
        type_parameter.insert_into_namespace_self(handler, struct_ctx.by_ref())?;
    }

    type_id.check_type_parameter_bounds(handler, struct_ctx, &span, None)?;

    let exp = ty::TyExpression {
        expression: ty::TyExpressionVariant::StructExpression {
            struct_ref,
            fields: typed_fields,
            instantiation_span: inner_span,
            call_path_binding,
        },
        return_type: type_id,
        span,
    };

    Ok(exp)
}

/// Type checks the field arguments.
fn type_check_field_arguments(
    handler: &Handler,
    mut ctx: TypeCheckContext,
    fields: &[StructExpressionField],
    struct_name: &Ident,
    struct_fields: &mut [ty::TyStructField],
    span: &Span,
    struct_decl_span: &Span,
    emit_missing_fields_error: bool
) -> Result<Vec<ty::TyStructExpressionField>, ErrorEmitted> {
    let type_engine = ctx.engines.te();
    let engines = ctx.engines();

    let mut typed_fields = vec![];
    let mut missing_fields = vec![];

    for struct_field in struct_fields.iter_mut() {
        match fields.iter().find(|x| x.name == struct_field.name) {
            Some(field) => {
                let ctx = ctx
                    .by_ref()
                    .with_help_text(UNIFY_STRUCT_FIELD_HELP_TEXT)
                    .with_type_annotation(struct_field.type_argument.type_id)
                    .with_unify_generic(true);
                let value = match ty::TyExpression::type_check(handler, ctx, field.value.clone()) {
                    Ok(res) => res,
                    Err(_) => continue,
                };
                typed_fields.push(ty::TyStructExpressionField {
                    value,
                    name: field.name.clone(),
                });
                struct_field.span = field.value.span.clone();
            }
            None => {
                missing_fields.push(struct_field.name.clone());

                let err = Handler::default().emit_err(CompileError::StructInstantiationMissingFieldForErrorRecovery {
                    field_name: struct_field.name.clone(),
                    struct_name: struct_name.clone(),
                    span: span.clone(),
                });

                typed_fields.push(ty::TyStructExpressionField {
                    name: struct_field.name.clone(),
                    value: ty::TyExpression {
                        expression: ty::TyExpressionVariant::Tuple { fields: vec![] },
                        return_type: type_engine.insert(
                            engines,
                            TypeInfo::ErrorRecovery(err),
                            None,
                        ),
                        span: span.clone(),
                    },
                });
            }
        }
    }

    if emit_missing_fields_error && !missing_fields.is_empty() {
        handler.emit_err(CompileError::StructInstantiationMissingFields {
            field_names: missing_fields,
            struct_name: struct_name.clone(),
            span: span.clone(),
            struct_decl_span: struct_decl_span.clone(),
            total_number_of_fields: struct_fields.len(),
        });
    }

    Ok(typed_fields)
}

/// Unifies the field arguments and the types of the fields from the struct
/// definition.
fn unify_field_arguments_and_struct_fields(
    handler: &Handler,
    ctx: TypeCheckContext,
    typed_fields: &[ty::TyStructExpressionField],
    struct_fields: &[ty::TyStructField],
) -> Result<(), ErrorEmitted> {
    let type_engine = ctx.engines.te();
    let engines = ctx.engines();

    handler.scope(|handler| {
        for struct_field in struct_fields.iter() {
            if let Some(typed_field) = typed_fields.iter().find(|x| x.name == struct_field.name) {
                type_engine.unify_with_generic(
                    handler,
                    engines,
                    typed_field.value.return_type,
                    struct_field.type_argument.type_id,
                    &typed_field.value.span,
                    UNIFY_STRUCT_FIELD_HELP_TEXT,
                    None,
                );
            }
        }
        Ok(())
    })
}
