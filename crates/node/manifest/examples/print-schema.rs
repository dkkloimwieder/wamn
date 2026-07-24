//! Regenerate the published JSON Schema contract from the canonical types:
//!
//! ```sh
//! cargo run -p wamn-node-manifest --example print-schema > docs/wamn-node-manifest.schema.json
//! ```
//!
//! `schema_drift` (tests/manifest.rs) fails if the committed file falls out of
//! sync with the types.

fn main() {
    print!("{}", wamn_node_manifest::json_schema_string());
}
