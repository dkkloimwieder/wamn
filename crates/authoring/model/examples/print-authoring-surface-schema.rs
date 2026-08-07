//! Regenerate the frontend-neutral authoring contract:
//!
//! ```sh
//! cargo run -p wamn-authoring-model --example print-authoring-surface-schema \
//!   > docs/contracts/authoring-surface.schema.json
//! ```

fn main() {
    print!("{}", wamn_authoring_model::json_schema_string());
}
