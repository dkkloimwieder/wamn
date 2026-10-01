//! Generated route entries and declaration entries.
//!
//! Every generated operation gets one route entry in
//! `generated/publication/attachments.json` and one operation entry in
//! `generated/publication/component-operations.json`, keyed by component. The
//! authored `publication/attachments.json` and component declarations carry
//! only the authored operations. Each reader reads both, and refuses an
//! attachment id or an operation that appears in both.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::{GenerateError, PackageManifest, insert_json_line};
use crate::manifest::canonical_operation_identity;
use crate::route_schema::{
    GENERATED_ATTACHMENTS, GENERATED_COMPONENT_OPERATIONS, GENERATED_ROUTES,
};

/// The body size limit of every generated route.
const RAW_BODY_MAXIMUM: u64 = 1_048_576;

/// Emit both publication files, when the package has a generated operation.
pub(super) fn emit_package_publication(
    files: &mut BTreeMap<String, Vec<u8>>,
    manifest: &PackageManifest,
) -> Result<(), GenerateError> {
    let package = &manifest.package;
    let routes = &manifest.routes;
    let id_prefix = routes.id_prefix.as_deref().unwrap_or("");
    let path_prefix = routes.path_prefix.as_deref().unwrap_or("");
    let modes = routes
        .auth_modes
        .clone()
        .unwrap_or_else(|| vec!["pat".to_owned(), "session".to_owned()]);
    let single = (manifest.components.len() == 1)
        .then(|| manifest.components.keys().next())
        .flatten();

    let mut attachments = BTreeMap::new();
    let mut declarations = BTreeMap::<&str, BTreeMap<String, Value>>::new();
    for (model_name, model) in &manifest.models {
        for (action, operation) in &model.operations {
            let action = action.as_str();
            let component = operation
                .component
                .as_ref()
                .or(single)
                .expect("manifest validation groups every operation in one component");
            let registered =
                canonical_operation_identity(package, &format!("{model_name}.{action}"))?;
            let schema = json!({"$ref": format!("{GENERATED_ROUTES}{model_name}/{action}.json")});
            let id = wamn_catalog::route_attachment_id(id_prefix, model_name, action);
            let definition = json!({
                "id": id,
                "type": "http",
                "route": {"path": wamn_catalog::route_path(path_prefix, model_name, action)},
                "input-schema": schema,
                "raw-body-bytes": {"maximum": RAW_BODY_MAXIMUM},
            });
            let hash = wamn_execution_contract::canonical_json_sha256(&definition);
            attachments.insert(
                id,
                json!({
                    "type": "http",
                    "package-id": package.id,
                    "component": component,
                    "operation": registered,
                    "definition-hash": hash,
                    "definition": definition,
                    "auth-policy": {"modes": modes},
                    "registered-operation": registered,
                }),
            );
            declarations.entry(component).or_default().insert(
                registered.clone(),
                json!({
                    "registered-operation": registered,
                    "input-ports": [{"name": "input", "schema": schema}],
                    "output-ports": [{"name": "main", "schema": {"type": "array"}}],
                    "parameters": [],
                }),
            );
        }
    }
    if attachments.is_empty() {
        return Ok(());
    }
    insert_json_line(files, GENERATED_ATTACHMENTS, &attachments)?;
    insert_json_line(files, GENERATED_COMPONENT_OPERATIONS, &declarations)
}
