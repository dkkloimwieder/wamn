//! Regenerate the published JSON Schema contract from the canonical types:
//!
//! ```sh
//! cargo run -p wamn-flow --example print-schema > docs/flow-schema.schema.json
//! ```
//!
//! `schema_drift` (tests/schema.rs) fails if the committed file falls out of
//! sync with the types.

fn main() {
    print!("{}", wamn_flow::json_schema_string());
}
