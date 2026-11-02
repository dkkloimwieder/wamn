use wamn_catalog::manifest::rust_identifier;

/// A field of a generated serde struct: the Rust spelling, renamed to the
/// authored name when the two differ.
pub(crate) fn serde_field(value: &str) -> Option<String> {
    rust_identifier(value).map(|rust| {
        if rust == value {
            rust
        } else {
            format!("#[serde(rename = {value:?})] {rust}")
        }
    })
}
