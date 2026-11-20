//! Write formatted Rust artifacts from tokens.

use proc_macro2::{Ident, TokenStream};
use quote::format_ident;
use wamn_package::manifest::binding_identifier;

use super::{BTreeMap, GenerateError, GenerateErrorType, insert_bytes};

pub(super) fn rust_ident(name: &str) -> Ident {
    let name = rust_name(name);
    format_ident!("{name}")
}

fn rust_name(name: &str) -> String {
    let name = name.replace(['-', '.'], "_");
    // The binding generator leaves this contextual keyword unchanged.
    if name == "union" {
        return name;
    }
    binding_identifier(&name).unwrap_or_else(|| [name.as_str(), "_"].concat())
}

pub(super) fn check_column_name(model: &str, column: &str) -> Result<(), GenerateError> {
    syn::parse_str::<Ident>(&rust_name(column))
        .map(drop)
        .map_err(|error| syntax_error(&format!("model {model} column {column}"), &error))
}

pub(super) fn insert_rust(
    files: &mut BTreeMap<String, Vec<u8>>,
    path: &str,
    header: &str,
    tokens: TokenStream,
) -> Result<(), GenerateError> {
    let syntax = syn::parse2::<syn::File>(tokens).map_err(|error| syntax_error(path, &error))?;
    let mut source = header.to_owned();
    source.push_str(&prettyplease::unparse(&syntax));
    insert_bytes(files, path, source.into_bytes())
}

fn syntax_error(context: &str, error: &syn::Error) -> GenerateError {
    GenerateError::with_source(
        GenerateErrorType::InvalidManifest,
        format!("internal Rust generation error in {context}"),
        std::io::Error::other(error.to_string()),
    )
}
