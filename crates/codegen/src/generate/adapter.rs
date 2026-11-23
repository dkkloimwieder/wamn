//! Generate Rust adapters for model and custom operations.

use proc_macro2::{Ident, Literal, TokenStream};
use quote::quote;

use super::BTreeMap;
use super::operations::{CustomDescription, ModelDescription, ModelOperation, ModelShape};
use super::rust_source::{check_column_name, insert_rust, rust_ident};
use super::typed_exports::{emit_error_mapper, emit_export_adapter, emit_row_adapter};
use super::wit::{access_error_literal, crud_is_keyed, custom_is_keyed, detail_name, model_column};
use crate::client_fields::FieldIr;
use wamn_package::manifest::ManifestError as GenerateError;
use wamn_package::manifest::{
    AccessOperationErrorLiteral, ContractFieldDeclaration, CrudAction, FilterMatch,
    OperationDeclaration, OperationErrorDetailDeclaration, OperationErrorDetailKey,
    rust_type_identifier,
};

const HEADER: &str = "// @generated from operation declarations; do not edit.\n";

pub(super) fn emit_model_adapters(
    files: &mut BTreeMap<String, Vec<u8>>,
    compiled: &ModelDescription<'_>,
) -> Result<(), GenerateError> {
    let model_name = compiled.name;
    for column in compiled.shape.columns {
        check_column_name(model_name.as_str(), column.name())?;
    }
    for (action, facts) in &compiled.operations {
        insert_rust(
            files,
            &format!(
                "generated/wit/{model_name}_{}_operation.rs",
                action.as_str()
            ),
            HEADER,
            emit_model_export(*action, compiled, facts),
        )?;
    }
    Ok(())
}

pub(super) fn emit_custom_adapter(
    files: &mut BTreeMap<String, Vec<u8>>,
    group: &str,
    local_name: &str,
    compiled: &CustomDescription<'_>,
) -> Result<(), GenerateError> {
    insert_rust(
        files,
        &format!("generated/wit/{group}_{local_name}_operation.rs"),
        HEADER,
        emit_custom_export(local_name, compiled),
    )
}

fn emit_model_export(
    action: CrudAction,
    compiled: &ModelDescription<'_>,
    facts: &ModelOperation<'_>,
) -> TokenStream {
    let name = rust_type_identifier(action.as_str());
    let mut source = support();
    let validation = emit_crud_validation(
        action,
        &compiled.shape,
        facts.request,
        &facts.result.fields,
        compiled.min_lengths,
    );
    if !validation.is_empty() || (action == CrudAction::Query && facts.request.limit.is_some()) {
        source.extend(invalid_detail(
            &facts.refusals[&AccessOperationErrorLiteral::InvalidInput],
        ));
    }
    let request = rust_ident(&[&name, "Request"].concat());
    source.extend(emit_normalizer(
        &request,
        &quote!(contract::InvalidInputDetail),
        &validation,
    ));
    if action == CrudAction::Query {
        source.extend(emit_query_limit(
            facts.request,
            compiled.feed,
            &facts.refusals,
        ));
        source.extend(query_export());
    } else {
        let item = rust_ident(&[&name, "Item"].concat());
        source.extend(export_prelude(&item, 1, 100, crud_is_keyed(action)));
        source.extend(emit_handler(&name, crud_is_keyed(action)));
        source.extend(emit_export_adapter(&name, false, action.changes_records()));
    }
    source.extend(emit_row_adapter(
        facts
            .result
            .fields
            .iter()
            .map(|f| (f.path.as_str(), f.ty, f.nullable, None)),
    ));
    source.extend(emit_error_mapper(
        &[&name, "Error"].concat(),
        facts
            .refusals
            .iter()
            .map(|(code, detail)| (access_error_literal(*code), detail)),
        facts.revision,
        &facts.guarded_fields,
    ));
    source
}

fn support() -> TokenStream {
    quote! {
        #[allow(unused_imports)]
        pub(crate) use wamn_guest::data_access::InputError;
    }
}

fn scalar_refusal(type_name: &str, value: &TokenStream) -> Option<TokenStream> {
    if type_name == "json" {
        return Some(quote! {
            wamn_guest::data_access::scalar::json(&(#value).encoded_json)
                .map(|parsed| { (#value).encoded_json = parsed.0; }).is_err()
        });
    }
    matches!(type_name, "uuid" | "numeric" | "timestamptz").then(|| {
        let parser = rust_ident(type_name);
        quote! {
            wamn_guest::data_access::scalar::#parser(#value)
                .map(|parsed| { *#value = parsed.0; }).is_err()
        }
    })
}

fn emit_query_limit(
    operation: &OperationDeclaration,
    feed: bool,
    details: &BTreeMap<AccessOperationErrorLiteral, OperationErrorDetailDeclaration>,
) -> TokenStream {
    let limits = operation.limit.as_ref().map(|limit| {
        let optional = &details[&AccessOperationErrorLiteral::InvalidInput].optional;
        let minimum_text = limit.minimum.to_string();
        let maximum_text = limit.maximum.to_string();
        let default = Literal::u32_unsuffixed(limit.default);
        let minimum = Literal::u32_unsuffixed(limit.minimum);
        let maximum = Literal::u32_unsuffixed(limit.maximum);
        let min_detail = optional
            .contains(&OperationErrorDetailKey::Minimum)
            .then(|| quote!(detail.minimum = Some(#minimum_text.to_owned());));
        let max_detail = optional
            .contains(&OperationErrorDetailKey::Maximum)
            .then(|| quote!(detail.maximum = Some(#maximum_text.to_owned());));
        let observed = optional
            .contains(&OperationErrorDetailKey::Observed)
            .then(|| quote!(detail.observed = Some(limit.to_string());));
        let validate = |access: TokenStream, path: &str| {
            quote! {
                let limit = *#access.get_or_insert(#default);
                if !(#minimum..=#maximum).contains(&limit) {
                    #[allow(unused_mut)]
                    let mut detail = invalid(#path);
                    #min_detail #max_detail #observed
                    return Err(detail);
                }
            }
        };
        let page = validate(quote!(request.limit), "limit");
        let feed = feed.then(|| {
            let validation = validate(quote!(feed.limit), "feed.limit");
            quote!(if let Some(feed) = &mut request.feed { #validation })
        });
        quote!(#page #feed)
    });
    quote! {
        #[allow(clippy::unnecessary_wraps)]
        fn limit(request: &mut contract::QueryRequest) -> Result<(), contract::InvalidInputDetail> {
            let _ = &request;
            #limits
            Ok(())
        }
    }
}

fn query_export() -> TokenStream {
    quote! {
        #[allow(dead_code)]
        pub(crate) async fn run<S, F>(
            request: Result<contract::QueryRequest, contract::InvalidInputDetail>,
            state: &mut S,
            handler: F,
        ) -> Result<contract::QueryResult, contract::QueryError>
        where
            F: AsyncFnMut(&mut S, contract::QueryRequest) -> Result<contract::QueryResult, contract::QueryError>,
        {
            wamn_guest::data_access::operation::call(request, state, |request| normalize(request).and_then(|()| limit(request)), contract::QueryError::InvalidInput, handler).await
        }
        pub(crate) type Input = contract::QueryRequest;
        pub(crate) type Output = contract::QueryOutcome;
        pub(crate) use wamn_guest::export_query as export_operation;
    }
}

fn emit_mutation_validation(
    table: &ModelShape<'_>,
    operation: &OperationDeclaration,
    fields: &[ContractFieldDeclaration],
    min_lengths: &BTreeMap<String, u32>,
    create: bool,
) -> TokenStream {
    let mut source = TokenStream::new();
    for field in &operation.writable_fields {
        let column = model_column(table, field);
        let name = rust_ident(field);
        let (access, path) = if create {
            (quote!(request.#name), field.to_owned())
        } else {
            (quote!(request.change.#name), ["change.", field].concat())
        };
        if create && super::omission_refused(column) {
            source.extend(quote!(if #access.is_none() { return Err(invalid(#path)); }));
        }
        if !column.nullable() {
            source.extend(quote!(if matches!(#access, Some(None)) { return Err(invalid(#path)); }));
        }
        let values = operation.values.get(field).map_or_else(
            || {
                fields
                    .iter()
                    .find(|item| item.path == *field)
                    .map_or(&[][..], |item| item.values.as_slice())
            },
            Vec::as_slice,
        );
        let mut conditions = Vec::new();
        if let Some(condition) = scalar_refusal(column.column_type().as_str(), &quote!(value)) {
            conditions.push(condition);
        }
        if !values.is_empty() {
            conditions.push(quote!(![#(#values),*].contains(&value.as_str())));
        }
        if let Some(&minimum) = min_lengths.get(field) {
            let minimum = Literal::u32_unsuffixed(minimum);
            conditions.push(quote!(value.trim().chars().count() < #minimum));
        }
        let mut conditions = conditions.into_iter();
        if let Some(first) = conditions.next() {
            source.extend(quote! {
                if let Some(Some(value)) = &mut #access && (#first #(|| #conditions)*) {
                    return Err(invalid(#path));
                }
            });
        }
    }
    source
}

fn emit_crud_validation(
    action: CrudAction,
    table: &ModelShape<'_>,
    operation: &OperationDeclaration,
    fields: &[ContractFieldDeclaration],
    min_lengths: &BTreeMap<String, u32>,
) -> TokenStream {
    let mut body = TokenStream::new();
    if (matches!(
        action,
        CrudAction::Get | CrudAction::Update | CrudAction::Delete
    ) || (action == CrudAction::Create && !table.generated_id))
        && table.key == crate::key::KeyType::Uuid
    {
        body.extend(quote! {
            request.id = wamn_guest::data_access::scalar::uuid(&request.id)
                .map_err(|_| invalid("id"))?.0;
        });
    }
    if matches!(action, CrudAction::Create | CrudAction::Update) {
        body.extend(emit_mutation_validation(
            table,
            operation,
            fields,
            min_lengths,
            action == CrudAction::Create,
        ));
    }
    if action == CrudAction::Query {
        for filter in &operation.filters {
            let column = model_column(table, &filter.field);
            let name = rust_ident(&filter.field);
            let path = ["filter.", &filter.field].concat();
            if let Some(condition) = scalar_refusal(column.column_type().as_str(), &quote!(value)) {
                if filter.match_mode.takes_list() {
                    body.extend(quote! {
                        if let Some(values) = &mut request.#name {
                            for value in values { if #condition { return Err(invalid(#path)); } }
                        }
                    });
                } else if filter.match_mode == FilterMatch::Range {
                    body.extend(quote! {
                        if let Some(range) = &mut request.#name {
                            for value in [&mut range.min, &mut range.max].into_iter().flatten() {
                                if #condition { return Err(invalid(#path)); }
                            }
                        }
                    });
                }
            }
            let values = fields
                .iter()
                .find(|item| item.path == filter.field)
                .map_or(&[][..], |item| item.values.as_slice());
            if !values.is_empty() && filter.match_mode.takes_list() {
                body.extend(quote! {
                    if let Some(values) = &request.#name {
                        for value in values {
                            if ![#(#values),*].contains(&value.as_str()) { return Err(invalid(#path)); }
                        }
                    }
                });
            }
        }
    }
    body
}

fn emit_normalizer(request: &Ident, error: &TokenStream, validation: &TokenStream) -> TokenStream {
    let (parameter, lint) = if validation.is_empty() {
        (
            quote!(_request),
            quote!(#[allow(clippy::unnecessary_wraps, reason = "dispatch expects the same result type for every request")]),
        )
    } else {
        (quote!(request), TokenStream::new())
    };
    quote! {
        #lint
        pub(crate) fn normalize(#parameter: &mut contract::#request) -> Result<(), #error> {
            #validation
            Ok(())
        }
    }
}

fn invalid_detail(detail: &OperationErrorDetailDeclaration) -> TokenStream {
    let required = detail
        .required
        .contains(&OperationErrorDetailKey::Field)
        .then(|| quote!(field: field.to_owned(),));
    let optional = detail
        .optional
        .iter()
        .map(|key| rust_ident(detail_name(*key)));
    quote! {
        fn invalid(field: &str) -> contract::InvalidInputDetail {
            contract::InvalidInputDetail { #required #(#optional: None,)* }
        }
    }
}

fn emit_handler(type_name: &str, keyed: bool) -> TokenStream {
    let (key, correlation, outcome_members) = if keyed {
        (
            quote!(item.request_id),
            quote!(request_id),
            quote!(request_id, outcome),
        )
    } else {
        (quote!(()), quote!(()), quote!(outcome))
    };
    let item = rust_ident(&[type_name, "Item"].concat());
    let outcome = rust_ident(&[type_name, "Outcome"].concat());
    let request = rust_ident(&[type_name, "Request"].concat());
    let result = rust_ident(&[type_name, "Result"].concat());
    let error = rust_ident(&[type_name, "Error"].concat());
    quote! {
        #[allow(dead_code)]
        pub(crate) async fn run<S, F>(input: Vec<contract::#item>, state: &mut S, handler: F) -> Vec<contract::#outcome>
        where
            F: AsyncFnMut(&mut S, contract::#request) -> Result<contract::#result, contract::#error>,
        {
            wamn_guest::data_access::operation::dispatch(input.into_iter().map(|item| (#key, item.input)), state, normalize, contract::#error::InvalidInput, handler)
                .await.into_iter().map(|(#correlation, outcome)| contract::#outcome { #outcome_members })
                .collect()
        }
    }
}

fn export_prelude(item: &Ident, minimum: u32, maximum: u32, keyed: bool) -> TokenStream {
    let minimum = Literal::u32_unsuffixed(minimum);
    let maximum = Literal::u32_unsuffixed(maximum);
    let correlations = if keyed {
        quote!(input.iter().map(|item| item.request_id.as_str()))
    } else {
        quote!(std::iter::empty())
    };
    quote! {
        #[allow(dead_code)]
        pub(crate) fn validate(input: &[contract::#item]) -> Result<(), InputError> {
            wamn_guest::data_access::operation::validate_envelope(input.len(), #minimum..=#maximum, #correlations)
        }
    }
}

fn emit_custom_export(local_name: &str, compiled: &CustomDescription<'_>) -> TokenStream {
    let operation = compiled.declaration;
    let name = rust_type_identifier(local_name);
    let request = rust_ident(&[&name, "Request"].concat());
    let mut source = support();
    let mut validation = emit_tree_validation(&compiled.request, &quote!(request));
    if let Some(result) = &compiled.result {
        let (min, max) = if operation.feed == Some(true) {
            (1, 1)
        } else {
            operation
                .input
                .envelope
                .as_ref()
                .map_or((1, 100), |bounds| (bounds.minimum, bounds.maximum))
        };
        let item = rust_ident(&[&name, "Item"].concat());
        source.extend(export_prelude(&item, min, max, custom_is_keyed(operation)));
        validation.extend(custom_limits(compiled));
        if !validation.is_empty() {
            source.extend(invalid_detail(&compiled.refusals["invalid_input"]));
        }
        source.extend(emit_normalizer(
            &request,
            &quote!(contract::InvalidInputDetail),
            &validation,
        ));
        source.extend(emit_handler(&name, custom_is_keyed(operation)));
        let error_name = [&name, "Error"].concat();
        source.extend(emit_row_adapter(result.fields.iter().map(|field| {
            let enum_error = (operation.feed == Some(true) && !field.values.is_empty())
                .then_some(error_name.as_str());
            (field.path.as_str(), field.ty, field.nullable, enum_error)
        })));
        source.extend(emit_error_mapper(
            &error_name,
            compiled
                .refusals
                .iter()
                .map(|(code, detail)| (code.as_str(), detail)),
            compiled.revision,
            &BTreeMap::new(),
        ));
        source.extend(emit_export_adapter(
            &name,
            false,
            operation.has_host_transaction(),
        ));
    } else {
        if !validation.is_empty() {
            source.extend(quote! {
                fn invalid(field: &'static str) -> InputError { InputError::new(field) }
            });
        }
        source.extend(emit_normalizer(&request, &quote!(InputError), &validation));
        source.extend(emit_export_adapter(&name, true, false));
    }
    source
}

fn custom_limits(compiled: &CustomDescription<'_>) -> TokenStream {
    let operation = compiled.declaration;
    let mut source = TokenStream::new();
    if operation.lists.is_some()
        && let Some(field) = compiled
            .request
            .iter()
            .find(|field| field.path == "limit" || field.path == "value.limit")
        && matches!(field.type_name.as_str(), "int32" | "int64")
    {
        let check = if field.required && !field.nullable {
            quote!(!(1..=100).contains(&request.limit))
        } else {
            quote!(
                request
                    .limit
                    .is_some_and(|limit| !(1..=100).contains(&limit))
            )
        };
        source.extend(quote!(if #check { return Err(invalid("limit")); }));
    }
    if operation.feed == Some(true) {
        source.extend(quote! {
            if let Some(part) = &mut request.feed {
                let limit = *part.limit.get_or_insert(100);
                if !(1..=100).contains(&limit) { return Err(invalid("feed.limit")); }
            }
        });
    }
    source
}

fn emit_tree_validation(fields: &[FieldIr], access: &TokenStream) -> TokenStream {
    let mut source = TokenStream::new();
    for field in fields {
        let path = &field.path;
        let member = rust_ident(
            path.rsplit('.')
                .next()
                .expect("declared field has a member")
                .trim_end_matches("[]"),
        );
        let field_access = quote!(#access.#member);
        if !field.children.is_empty() {
            if field.type_name == "array" || path.ends_with("[]") {
                if let Some(minimum) = field.minimum {
                    let check = if minimum == 1 {
                        quote!(#field_access.is_empty())
                    } else {
                        let minimum = Literal::u64_unsuffixed(minimum);
                        quote!(#field_access.len() < #minimum)
                    };
                    source.extend(quote!(if #check { return Err(invalid(#path)); }));
                }
                if let Some(maximum) = field.maximum {
                    let maximum = Literal::u64_unsuffixed(maximum);
                    source.extend(
                        quote!(if #field_access.len() > #maximum { return Err(invalid(#path)); }),
                    );
                }
                let children = emit_tree_validation(&field.children, &quote!(value));
                if !children.is_empty() {
                    source.extend(quote!(for value in &mut #field_access { #children }));
                }
            } else {
                source.extend(emit_tree_validation(&field.children, &field_access));
            }
            continue;
        }
        let Some(condition) = scalar_refusal(&field.type_name, &quote!(value)) else {
            continue;
        };
        if field.nullable {
            source.extend(quote! {
                if let Some(value) = &mut #field_access && (#condition) { return Err(invalid(#path)); }
            });
        } else {
            source.extend(quote! {
                {
                    let value = &mut #field_access;
                    if #condition { return Err(invalid(#path)); }
                }
            });
        }
    }
    source
}
