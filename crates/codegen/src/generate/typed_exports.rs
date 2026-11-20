//! Standard adapters between application values and declared typed contracts.

use proc_macro2::TokenStream;
use quote::quote;
use wamn_scalar::SqlValueType;

use super::rust_source::rust_ident;
use super::wit::detail_name;
use wamn_catalog::manifest::{
    OperationErrorDetailDeclaration, OperationErrorDetailKey, rust_type_identifier,
};

/// Emit a row conversion without requiring an application crate dependency.
pub(super) fn emit_row_adapter<'a>(
    fields: impl IntoIterator<Item = (&'a str, SqlValueType, bool)>,
) -> TokenStream {
    let members = fields.into_iter().filter_map(|(path, ty, nullable)| {
        let list = path.strip_suffix("[]");
        let name = list.unwrap_or(path);
        // The WIT record includes only top-level result fields.
        if name.contains('.') || name.contains("[]") {
            return None;
        }
        let member = rust_ident(name);
        let convert = |value: TokenStream| match ty {
            SqlValueType::Uuid | SqlValueType::Numeric | SqlValueType::Timestamptz => {
                quote!(#value.0)
            }
            SqlValueType::Json => {
                quote!(crate::wamn::node::types::JsonValue { encoded_json: #value.0 })
            }
            _ => value,
        };
        let input = quote!(row.#member);
        let requires_conversion = matches!(
            ty,
            SqlValueType::Uuid
                | SqlValueType::Numeric
                | SqlValueType::Timestamptz
                | SqlValueType::Json
        );
        let value = if !requires_conversion {
            input
        } else if list.is_some() {
            let value = convert(quote!(value));
            if nullable {
                quote!(#input.into_iter().map(|value| value.map(|value| #value)).collect())
            } else {
                quote!(#input.into_iter().map(|value| #value).collect())
            }
        } else if nullable {
            let value = convert(quote!(value));
            quote!(#input.map(|value| #value))
        } else {
            convert(input)
        };
        Some(quote!(#member: #value,))
    });
    quote! {
        #[allow(unused_macros)]
        macro_rules! row {
            ($row:expr, $target:path) => {{
                let row = $row;
                $target { #(#members)* }
            }};
        }
        #[allow(unused_imports)]
        pub(crate) use row;
    }
}

/// Emit the declared error vocabulary and expose only permitted details.
pub(super) fn emit_error_mapper<'a>(
    error_type: &str,
    cases: impl IntoIterator<Item = (&'a str, &'a OperationErrorDetailDeclaration)>,
    revision: Option<SqlValueType>,
    guarded_fields: &std::collections::BTreeMap<String, String>,
) -> TokenStream {
    let revision_type = if revision == Some(SqlValueType::Int32) {
        quote!(i32)
    } else {
        quote!(i64)
    };
    let error_type = rust_ident(error_type);
    let arms = cases.into_iter().filter(|(literal, _)| *literal != "internal_error").map(|(literal, declaration)| {
        let variant = rust_type_identifier(literal);
        let detail_type = rust_ident(&[&variant, "Detail"].concat());
        let variant = rust_ident(&variant);
        let keys = declaration.required.iter().chain(&declaration.optional);
        let names = keys.clone().map(|key| rust_ident(detail_name(*key)));
        let bindings = keys.map(|key| {
            let name = rust_ident(detail_name(*key));
            let literal = name.to_string();
            let required = declaration.required.contains(key);
            let mut value = quote!(detail(#literal));
            if *key == OperationErrorDetailKey::Field
                && !required
                && declaration.required.contains(&OperationErrorDetailKey::Constraint)
                && !guarded_fields.is_empty()
            {
                let fallback = guarded_fields.iter().map(|(constraint, field)| quote!(#constraint => Some(#field.to_owned()),));
                value.extend(quote!(.or_else(|| match constraint.as_str() { #(#fallback)* _ => None })));
            }
            let numeric = matches!(key, OperationErrorDetailKey::ExpectedRowVersion | OperationErrorDetailKey::ObservedRowVersion);
            let internal = quote!(return contract::#error_type::InternalError;);
            match (required, numeric) {
                (true, true) => quote!(let Some(#name) = #value.and_then(|value| value.parse::<#revision_type>().ok()) else { #internal };),
                (true, false) => quote!(let Some(#name) = #value else { #internal };),
                (false, true) => quote!(let Ok(#name) = #value.map(|value| value.parse::<#revision_type>()).transpose() else { #internal };),
                (false, false) => quote!(let #name = #value;),
            }
        });
        let result = if declaration.required.is_empty() && declaration.optional.is_empty() {
            quote!(contract::#error_type::#variant)
        } else {
            quote!(contract::#error_type::#variant(contract::#detail_type { #(#names,)* }))
        };
        quote!(#literal => { #(#bindings)* #result })
    });
    quote! {
        #[allow(dead_code)]
        pub(crate) fn map_error(code: &str, mut detail: impl FnMut(&str) -> Option<String>) -> contract::#error_type {
            match code {
                #(#arms)*
                _ => contract::#error_type::InternalError,
            }
        }
    }
}

/// Emit an operation export that validates before calling the application.
pub(super) fn emit_export_adapter(type_name: &str, direct: bool, transacted: bool) -> TokenStream {
    let (input, output, error, export) = if direct {
        let request = rust_ident(&[type_name, "Request"].concat());
        (
            quote!(contract::#request),
            quote!(contract::#request),
            None,
            quote!(export_direct),
        )
    } else {
        let item = rust_ident(&[type_name, "Item"].concat());
        let outcome = rust_ident(&[type_name, "Outcome"].concat());
        let error = rust_ident(&[type_name, "Error"].concat());
        let export = if transacted {
            quote!(export_transaction)
        } else {
            quote!(export_batch)
        };
        (
            quote!(Vec<contract::#item>),
            quote!(Vec<contract::#outcome>),
            Some(quote!(#[allow(dead_code)] pub(crate) type OperationError = contract::#error;)),
            export,
        )
    };
    quote! {
        pub(crate) type Input = #input;
        pub(crate) type Output = #output;
        #error
        pub(crate) use wamn_guest::#export as export_operation;
    }
}
