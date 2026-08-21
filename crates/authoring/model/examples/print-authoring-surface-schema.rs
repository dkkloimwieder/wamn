//! Regenerate the frontend-neutral authoring contract:
//!
//! ```sh
//! cargo run -p wamn-authoring-model --example print-authoring-surface-schema
//! ```

fn main() {
    print!("{}", wamn_authoring_model::json_schema_string());
}
