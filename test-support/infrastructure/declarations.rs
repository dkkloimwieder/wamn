//! Typed component declarations and authoring requests for application tests.

use anyhow::Context as _;
use serde_json::Value;
use wamn_authoring_model::{
    AuthoringCommand, AuthoringDocument, AuthoringRequest, AuthoringRequestEnvelope,
    AuthoringScope, Gate, SCHEMA_VERSION,
};
use wamn_catalog::{ComponentDeclaration, ComponentPackageScope, PackageCoordinate};

/// Inputs for one request to the existing authoring gate.
#[derive(Debug, Clone)]
pub struct GateInput {
    pub command_id: String,
    pub package: PackageCoordinate,
    pub scope: AuthoringScope,
}

/// Fill only the declared scope and connection fields of a component template.
pub fn render_component_declaration(
    template: &str,
    scope: &ComponentPackageScope,
    store_alias: &str,
) -> anyhow::Result<ComponentDeclaration> {
    let scope =
        ComponentPackageScope::new(&scope.tenant_id, &scope.package_id, &scope.package_version)
            .context("component scope requires a tenant and a valid package coordinate")?;
    // The public scope parser requires a real package coordinate. Fill its
    // named template fields before parsing the complete public declaration.
    let mut document: Value =
        serde_json::from_str(template).context("parse component declaration template")?;
    let target_scope = document
        .get_mut("scope")
        .and_then(Value::as_object_mut)
        .context("component declaration template requires scope")?;
    for (field, placeholder, value) in [
        ("tenant-id", "__TENANT_ID__", scope.tenant_id.as_str()),
        ("package-id", "__PACKAGE_ID__", scope.package_id.as_str()),
        (
            "package-version",
            "__PACKAGE_VERSION__",
            scope.package_version.as_str(),
        ),
    ] {
        let target = target_scope
            .get_mut(field)
            .with_context(|| format!("component declaration scope is missing {field}"))?;
        if target.as_str() == Some(placeholder) {
            *target = Value::from(value);
        }
    }
    if let Some(connections) = document
        .get_mut("connections")
        .and_then(Value::as_array_mut)
    {
        for connection in connections {
            if let Some(alias) = connection.get_mut("store-alias") {
                if alias.as_str() == Some("__STORE_ALIAS__") {
                    *alias = Value::from(store_alias);
                }
            }
        }
    }
    if let Some(placeholder) = remaining_placeholder(&document) {
        anyhow::bail!("component declaration still carries the placeholder {placeholder}");
    }
    serde_json::from_value(document).context("parse rendered public component declaration")
}

/// Embed exactly one parsed wiring document in the public authoring request.
pub fn gate_document(input: &GateInput, wiring_json: &str) -> anyhow::Result<AuthoringDocument> {
    let document = serde_json::from_str(wiring_json).context("wiring must be one JSON document")?;
    Ok(AuthoringDocument::Request(Box::new(
        AuthoringRequestEnvelope::Command(AuthoringRequest {
            schema_version: SCHEMA_VERSION.to_owned(),
            command_id: input.command_id.clone(),
            command: AuthoringCommand::Gate(Gate {
                scope: input.scope.clone(),
                package_id: input.package.package_id().to_owned(),
                package_version: input.package.package_version().to_owned(),
                document,
            }),
        }),
    )))
}

fn remaining_placeholder(value: &Value) -> Option<&str> {
    match value {
        Value::String(value) => placeholder(value),
        Value::Array(values) => values.iter().find_map(remaining_placeholder),
        Value::Object(values) => values
            .iter()
            .find_map(|(key, value)| placeholder(key).or_else(|| remaining_placeholder(value))),
        _ => None,
    }
}

fn placeholder(value: &str) -> Option<&str> {
    for (start, pair) in value.as_bytes().windows(2).enumerate() {
        if pair != b"__" {
            continue;
        }
        let remaining = &value[start + 2..];
        let length = remaining
            .bytes()
            .take_while(|byte| byte.is_ascii_uppercase() || *byte == b'_')
            .count();
        if let Some(end) = remaining[..length].rfind("__") {
            if end > 0 {
                return Some(&value[start..start + end + 4]);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const WMS: &str = include_str!("../../apps/wamn_wms/publication/components/wms.json.in");
    const LABEL: &str = include_str!("../../apps/platform/no-std/label-render/declaration.json.in");
    const BLOB: &str = include_str!("../../apps/platform/execution/blob-put/declaration.json.in");
    const WIRING: &str =
        include_str!("../../apps/wamn_wms/publication/wirings/inventory_move_and_label.json");

    fn scope() -> ComponentPackageScope {
        ComponentPackageScope::new("quay-9-route-auth", "wamn_test", "7.7.7").unwrap()
    }

    #[test]
    fn application_coordinate_stays_owned_and_shared_components_take_the_caller_coordinate() {
        let scope = scope();
        for template in [WMS, LABEL, BLOB] {
            let declaration = render_component_declaration(template, &scope, "crate-9").unwrap();
            assert_eq!(declaration.scope.tenant_id, scope.tenant_id);
            if declaration.component == "wms" {
                assert_eq!(declaration.scope.package_id, "wamn_wms");
                assert_eq!(declaration.scope.package_version, "1.0.0");
            } else {
                assert_eq!(declaration.scope.package_id, scope.package_id);
                assert_eq!(declaration.scope.package_version, scope.package_version);
            }
            assert!(remaining_placeholder(&serde_json::to_value(&declaration).unwrap()).is_none());
            if declaration.component == "blob-put" {
                assert_eq!(declaration.connections.len(), 1);
                assert_eq!(declaration.connections[0].store_alias, "crate-9");
            }
        }
    }

    #[test]
    fn declaration_keeps_all_operation_facts_and_literal_connection_aliases() {
        let rendered = render_component_declaration(WMS, &scope(), "").unwrap();
        let original: Value = serde_json::from_str(WMS).unwrap();
        assert_eq!(
            serde_json::to_value(&rendered).unwrap()["operations"],
            original["operations"]
        );
        let mut template: Value = serde_json::from_str(BLOB).unwrap();
        template["connections"][0]["store-alias"] = Value::from("owned-alias");
        let rendered = render_component_declaration(
            &serde_json::to_string(&template).unwrap(),
            &scope(),
            "caller-alias",
        )
        .unwrap();
        assert_eq!(rendered.connections[0].store_alias, "owned-alias");
    }

    #[test]
    fn unknown_template_fields_and_unfilled_placeholders_are_refused_by_name() {
        let mut template: Value = serde_json::from_str(BLOB).unwrap();
        template["connections"][0]["other"] = Value::from("__LEGACY_KEY__");
        let error = render_component_declaration(
            &serde_json::to_string(&template).unwrap(),
            &scope(),
            "alias",
        )
        .unwrap_err();
        assert!(error.to_string().contains("__LEGACY_KEY__"));
        template["connections"][0]["other"] = Value::from("literal");
        let error = render_component_declaration(
            &serde_json::to_string(&template).unwrap(),
            &scope(),
            "alias",
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("other"));
        template.as_object_mut().unwrap().remove("scope");
        assert!(
            render_component_declaration(
                &serde_json::to_string(&template).unwrap(),
                &scope(),
                "alias"
            )
            .unwrap_err()
            .to_string()
            .contains("requires scope")
        );
    }

    #[test]
    fn unresolved_template_names_keep_the_existing_character_rules() {
        for (text, expected) in [
            ("_____", Some("_____")),
            ("____", None),
            ("__A__", Some("__A__")),
            ("___A__", Some("___A__")),
            ("__A___B__", Some("__A___B__")),
            ("__lower__", None),
            ("before __A__ after", Some("__A__")),
            ("é__A__", Some("__A__")),
        ] {
            assert_eq!(placeholder(text), expected, "{text}");
        }
    }

    #[test]
    fn known_placeholders_outside_scope_and_connections_are_not_rewritten() {
        let mut template: Value = serde_json::from_str(LABEL).unwrap();
        template["operations"]["wamn:node/handler@0.1.0"]["input-ports"][0]["schema"]["description"] =
            Value::from("__TENANT_ID__");
        let error =
            render_component_declaration(&serde_json::to_string(&template).unwrap(), &scope(), "")
                .unwrap_err();
        assert!(error.to_string().contains("__TENANT_ID__"));
    }

    #[test]
    fn invalid_scope_and_json_are_refused_and_values_are_not_text_substitutions() {
        let mut invalid = scope();
        invalid.tenant_id.clear();
        assert!(render_component_declaration(WMS, &invalid, "").is_err());
        assert!(render_component_declaration("not JSON", &scope(), "").is_err());
        let mut scope = scope();
        scope.tenant_id = "tenant/with&characters".into();
        let alias = "store/with&characters";
        let declaration = render_component_declaration(BLOB, &scope, alias).unwrap();
        assert_eq!(declaration.scope.tenant_id, scope.tenant_id);
        assert_eq!(declaration.connections[0].store_alias, alias);
    }

    fn gate_input() -> GateInput {
        GateInput {
            command_id: "gate-wamn_wms-inventory_move_and_label".into(),
            package: PackageCoordinate::new("wamn_wms", "1.0.0").unwrap(),
            scope: AuthoringScope {
                project_id: "wms".into(),
                environment: "dev".into(),
            },
        }
    }

    #[test]
    fn gate_request_uses_the_public_contract_and_keeps_the_complete_wiring() {
        let input = gate_input();
        let rendered = gate_document(&input, WIRING).unwrap();
        let AuthoringDocument::Request(request) = &rendered else {
            panic!("request")
        };
        let AuthoringRequestEnvelope::Command(request) = request.as_ref() else {
            panic!("command")
        };
        assert_eq!(request.schema_version, SCHEMA_VERSION);
        assert_eq!(request.command_id, input.command_id);
        let AuthoringCommand::Gate(gate) = &request.command else {
            panic!("gate")
        };
        assert_eq!(gate.scope, input.scope);
        assert_eq!(gate.package_id, "wamn_wms");
        assert_eq!(gate.package_version, "1.0.0");
        assert_eq!(
            gate.document,
            serde_json::from_str::<Value>(WIRING).unwrap()
        );
        let serialized = serde_json::to_string(&rendered).unwrap();
        assert!(wamn_authoring_model::decode_document(&serialized).is_ok());
    }

    #[test]
    fn gate_refuses_malformed_or_multiple_json_values() {
        for invalid in ["not JSON", "{\"a\":1}\n{\"b\":2}\n", ""] {
            assert!(
                gate_document(&gate_input(), invalid)
                    .unwrap_err()
                    .to_string()
                    .contains("one JSON document")
            );
        }
    }
}
