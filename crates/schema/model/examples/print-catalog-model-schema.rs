//! Regenerate the published JSON Schema contract from the canonical types:
//!
//! ```sh
//! cargo run -p wamn-schema-model --example print-catalog-model-schema > docs/reference/contracts/catalog-model.schema.json
//! ```
//!
//! `committed_schema_matches_types` (tests/catalog.rs) fails if the committed
//! file falls out of sync with the types.

fn main() {
    print!("{}", wamn_schema_model::json_schema_string());
}
