//! Served-route evidence from exact publication declarations.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_catalog::{
    ComponentDeclaration, ServingAttachment, WiringDocument, WiringNode, WiringTerminal,
    partial_response_schema,
};

use crate::client_ir::{ClientIrError, ClientIrErrorKind};

#[derive(Debug, Default)]
pub(super) struct RouteEvidence {
    pub input_schema: Option<Value>,
    pub output_schema: Option<Value>,
    pub partial_schema: Option<Value>,
    pub terminal_operation: Option<String>,
    pub direct: bool,
}

/// Read the request schema and the selected wiring's actual response owner.
pub(super) fn evidence(
    attachments: &Path,
    attachment: &ServingAttachment,
) -> Result<RouteEvidence, ClientIrError> {
    let definition = attachment
        .definition
        .as_object()
        .ok_or_else(|| malformed(attachments, "attachment definition must be an object"))?;
    let mut result = RouteEvidence {
        input_schema: definition
            .get("input-schema")
            .map(|schema| read_schema(attachments, "input-schema", schema))
            .transpose()?,
        ..RouteEvidence::default()
    };
    let publication = attachments.parent().unwrap_or_else(|| Path::new(""));
    let Some(wiring) = selected_wiring(publication, attachment)? else {
        return Ok(result);
    };
    let Some((terminal_id, terminal)) = response_node(&wiring) else {
        return Ok(result);
    };
    result.direct = wiring.nodes.len() == 1
        && wiring.edges.is_empty()
        && terminal.operation_dependency.is_none()
        && attachment.registered_operation.as_deref() == Some(terminal.operation.as_str());
    result.terminal_operation = Some(terminal.operation.clone());
    result.output_schema = if let Some(response) = &wiring.response {
        if response.node != terminal_id {
            return Err(malformed(
                publication,
                "the declared response is not the reachable Respond terminal",
            ));
        }
        if let Some(committed) = &response.committed_result {
            result.partial_schema =
                component_schema(publication, attachment, &wiring.nodes[committed], true)?
                    .as_ref()
                    .map(partial_response_schema);
        }
        Some(read_schema(
            publication,
            "wiring response schema",
            &response.schema,
        )?)
    } else {
        component_schema(publication, attachment, terminal, false)?
    };
    Ok(result)
}

fn selected_wiring(
    publication: &Path,
    attachment: &ServingAttachment,
) -> Result<Option<WiringDocument>, ClientIrError> {
    let mut selected = None;
    for path in declaration_paths(&publication.join("wirings"), false)? {
        let document = read_json(&path)?;
        let wiring = WiringDocument::parse(&document)
            .map_err(|error| malformed(&path, format!("invalid wiring: {error}")))?;
        if wiring.wiring_id != attachment.wiring_id || wiring.version != attachment.wiring_version {
            continue;
        }
        if selected.is_some() {
            return Err(malformed(
                &path,
                format!(
                    "more than one publication declares wiring {:?} version {}",
                    attachment.wiring_id, attachment.wiring_version
                ),
            ));
        }
        selected = Some(wiring);
    }
    Ok(selected)
}

fn response_node(wiring: &WiringDocument) -> Option<(&str, &WiringNode)> {
    let mut pending = vec![wiring.entry.as_str()];
    let mut reached = BTreeSet::new();
    let mut response = None;
    while let Some(node_id) = pending.pop() {
        if !reached.insert(node_id) {
            continue;
        }
        let node = &wiring.nodes[node_id];
        if node.terminal == Some(WiringTerminal::Respond) {
            if response.is_some() {
                return None;
            }
            response = Some((node_id, node));
        }
        pending.extend(
            wiring
                .edges
                .iter()
                .filter(|edge| edge.from == node_id)
                .map(|edge| edge.to.as_str()),
        );
    }
    response
}

fn component_schema(
    publication: &Path,
    attachment: &ServingAttachment,
    terminal: &WiringNode,
    committed: bool,
) -> Result<Option<Value>, ClientIrError> {
    // A dependency alias needs its owner's exact publication closure. The
    // attachment's package declarations do not establish that ownership.
    if terminal.operation_dependency.is_some() {
        return Ok(None);
    }
    let mut selected = None;
    let mut matched = false;
    for path in declaration_paths(&publication.join("components"), true)? {
        let declaration: ComponentDeclaration = serde_json::from_value(read_json(&path)?)
            .map_err(|error| malformed(&path, format!("invalid component declaration: {error}")))?;
        if declaration.scope.package_id != attachment.package_id
            || declaration.component != terminal.component
            || declaration.interface_version != terminal.interface_version
        {
            continue;
        }
        let Some(operation) = declaration.operations.get(&terminal.operation) else {
            continue;
        };
        if matched {
            return Ok(None);
        }
        matched = true;
        if committed {
            if operation.registered_operation.as_deref() != Some(terminal.operation.as_str()) {
                continue;
            }
            selected = operation
                .committed_result_schema
                .as_ref()
                .map(|schema| read_schema(&path, "committed result schema", schema))
                .transpose()?;
            continue;
        }
        let mut main = operation
            .output_ports
            .iter()
            .filter(|port| port.name == wamn_execution_contract::MAIN_PORT);
        if let Some(port) = main.next() {
            selected = Some(read_schema(&path, "main output-port schema", &port.schema)?);
        }
        if main.next().is_some() {
            return Err(malformed(
                &path,
                "the operation declares more than one main output port",
            ));
        }
    }
    Ok(selected)
}

#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "publication declaration suffixes are exact package paths"
)]
fn declaration_paths(directory: &Path, templates: bool) -> Result<Vec<PathBuf>, ClientIrError> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(unreadable(directory, &error)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| unreadable(directory, &error))?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.ends_with(".json") || (templates && name.ends_with(".json.in"))
            })
        {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn read_json(path: &Path) -> Result<Value, ClientIrError> {
    let bytes = std::fs::read(path).map_err(|error| unreadable(path, &error))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| malformed(path, format!("invalid JSON: {error}")))
}

fn read_schema(path: &Path, field: &str, schema: &Value) -> Result<Value, ClientIrError> {
    if !schema.is_object() && !schema.is_boolean() {
        return Err(malformed(
            path,
            format!("{field} must be an object or boolean"),
        ));
    }
    Ok(schema.clone())
}

fn malformed(path: &Path, detail: impl std::fmt::Display) -> ClientIrError {
    ClientIrError::new(
        ClientIrErrorKind::MalformedContract,
        format!("{}: {detail}", path.display()),
    )
}

fn unreadable(path: &Path, error: &std::io::Error) -> ClientIrError {
    ClientIrError::new(
        ClientIrErrorKind::UnreadableProjection,
        format!("read {}: {error}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::{Value, json};
    use wamn_catalog::{ServingAttachment, WiringDocument};

    use super::{evidence, response_node};
    use crate::client_ir::ClientIrErrorKind;

    struct Publication(PathBuf);

    impl Publication {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "wamn-client-route-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).expect("create route fixture");
            Self(root)
        }

        fn write(&self, relative: &str, value: &Value) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().expect("fixture parent"))
                .expect("create publication directory");
            std::fs::write(path, serde_json::to_vec(value).expect("serialize fixture"))
                .expect("write declaration");
        }

        fn attachments(&self) -> PathBuf {
            self.0.join("attachments.json")
        }
    }

    impl Drop for Publication {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn package_attachment(package: &str, id: &str) -> (PathBuf, ServingAttachment) {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps")
            .join(package)
            .join("publication/attachments.json");
        let attachments: BTreeMap<String, ServingAttachment> =
            serde_json::from_slice(&std::fs::read(&path).expect("read shipped attachments"))
                .expect("parse shipped attachments");
        (
            path,
            attachments.get(id).expect("shipped attachment").clone(),
        )
    }

    fn direct_wiring(attachment: &ServingAttachment) -> Value {
        json!({
            "format-version": "0.1",
            "wiring-id": attachment.wiring_id,
            "version": attachment.wiring_version,
            "entry": "operation",
            "nodes": {"operation": {
                "component": "receiving",
                "interface-version": "0.1.0",
                "operation": attachment.registered_operation,
                "terminal": "respond"
            }}
        })
    }

    #[test]
    fn receiving_claim_route_has_a_direct_declared_response() {
        let (path, attachment) =
            package_attachment("wamn_receiving", "receiving-record-receipt-http");
        let result = evidence(&path, &attachment).expect("read direct publication evidence");
        assert!(result.direct);
        assert_eq!(result.terminal_operation, attachment.registered_operation);
        assert_eq!(
            result.input_schema,
            Some(attachment.definition["input-schema"].clone())
        );
        assert_eq!(result.output_schema, Some(json!({"type": "array"})));
    }

    #[test]
    fn wms_move_response_belongs_to_the_terminal_store() {
        let (path, attachment) = package_attachment("wamn_wms", "inventory-move-http");
        let result = evidence(&path, &attachment).expect("read composed publication evidence");
        assert!(!result.direct);
        assert_eq!(
            result.terminal_operation.as_deref(),
            Some("wamn:node/async-handler@0.1.0")
        );
        let schema = result.output_schema.expect("declared terminal response");
        assert_eq!(
            schema["items"]["properties"]["value"]["properties"]["stored"]["properties"]["key"]["type"],
            "string"
        );
        let partial = result.partial_schema.expect("declared committed result");
        assert!(
            partial["properties"]["committed_result"]["items"]["properties"]["value"]["properties"]
                .get("movement_id")
                .is_some()
        );
        assert!(
            partial["properties"]["committed_result"]["items"]["properties"]["value"]["properties"]
                .get("stored")
                .is_none()
        );
    }

    #[test]
    fn wiring_selection_uses_declared_identity_and_version() {
        let (_, attachment) = package_attachment("wamn_receiving", "receiving-record-receipt-http");
        let fixture = Publication::new();
        let mut unrelated = direct_wiring(&attachment);
        unrelated["version"] = json!(attachment.wiring_version + 1);
        fixture.write("wirings/receiving_record_receipt.json", &unrelated);
        fixture.write(
            "wirings/unrelated-file-name.json",
            &direct_wiring(&attachment),
        );
        let result =
            evidence(&fixture.attachments(), &attachment).expect("read exact selected wiring");
        assert!(result.direct);
        assert_eq!(result.terminal_operation, attachment.registered_operation);
        assert_eq!(result.output_schema, None);
    }

    #[test]
    fn missing_wiring_preserves_input_without_inventing_response_evidence() {
        let (_, attachment) = package_attachment("wamn_receiving", "receiving-record-receipt-http");
        let fixture = Publication::new();
        let result =
            evidence(&fixture.attachments(), &attachment).expect("missing wiring is unknown");
        assert!(!result.direct);
        assert_eq!(result.terminal_operation, None);
        assert_eq!(result.output_schema, None);
        assert_eq!(
            result.input_schema,
            Some(attachment.definition["input-schema"].clone())
        );
    }

    #[test]
    fn malformed_schema_and_wiring_are_contextual_refusals() {
        let (_, mut attachment) =
            package_attachment("wamn_receiving", "receiving-record-receipt-http");
        let fixture = Publication::new();
        attachment.definition["input-schema"] = json!([]);
        let error =
            evidence(&fixture.attachments(), &attachment).expect_err("refuse malformed schema");
        assert_eq!(error.kind(), ClientIrErrorKind::MalformedContract);
        assert!(error.to_string().contains("attachments.json"));
        attachment.definition["input-schema"] = json!(true);
        let mut wiring = direct_wiring(&attachment);
        wiring["entry"] = json!("absent");
        fixture.write("wirings/invalid.json", &wiring);
        let error =
            evidence(&fixture.attachments(), &attachment).expect_err("refuse malformed wiring");
        assert_eq!(error.kind(), ClientIrErrorKind::MalformedContract);
        assert!(error.to_string().contains("invalid.json"));
    }

    #[test]
    fn response_selection_follows_reachability_and_refuses_ambiguous_terminals() {
        let (_, attachment) = package_attachment("wamn_receiving", "receiving-record-receipt-http");
        let mut wiring = direct_wiring(&attachment);
        wiring["nodes"]["other"] = wiring["nodes"]["operation"].clone();
        let parsed = WiringDocument::parse(&wiring).expect("parse unreachable terminal");
        assert_eq!(
            response_node(&parsed),
            Some(("operation", &parsed.nodes["operation"]))
        );
        wiring["edges"] = json!([{"from": "operation", "to": "other"}]);
        let parsed = WiringDocument::parse(&wiring).expect("parse two reachable terminals");
        assert_eq!(response_node(&parsed), None);
    }

    #[test]
    fn response_schema_requires_matching_publication_identity_and_preserves_opaque_arrays() {
        let (_, attachment) = package_attachment("wamn_receiving", "receiving-record-receipt-http");
        let fixture = Publication::new();
        fixture.write("wirings/route.json", &direct_wiring(&attachment));
        let operation = attachment
            .registered_operation
            .as_deref()
            .expect("registered operation");
        let mut declaration = json!({
            "scope": {"tenant-id": "tenant-a", "package-id": "other", "package-version": "1.0.0"},
            "component": "receiving",
            "interface-version": "0.1.0",
            "operations": {(operation): {
                "registered-operation": operation,
                "input-ports": [],
                "output-ports": [{"name": "main", "schema": {"type": "array"}}],
                "parameters": []
            }},
            "connections": []
        });
        fixture.write("components/declaration.json.in", &declaration);
        let result =
            evidence(&fixture.attachments(), &attachment).expect("unrelated package declaration");
        assert_eq!(result.output_schema, None);
        declaration["scope"]["package-id"] = json!(attachment.package_id);
        fixture.write("components/declaration.json.in", &declaration);
        let result =
            evidence(&fixture.attachments(), &attachment).expect("matching package declaration");
        assert_eq!(result.output_schema, Some(json!({"type": "array"})));
    }

    #[test]
    fn partial_projection_requires_one_exact_committed_component_declaration() {
        let (path, attachment) = package_attachment("wamn_wms", "inventory-move-http");
        let publication = path.parent().unwrap();
        let wiring =
            super::read_json(&publication.join("wirings/inventory_move_and_label.json")).unwrap();
        let declaration = super::read_json(&publication.join("components/wms.json.in")).unwrap();
        let fixture = Publication::new();
        fixture.write("wirings/composed.json", &wiring);
        let result = evidence(&fixture.attachments(), &attachment).unwrap();
        assert!(result.output_schema.is_some());
        assert!(
            result.partial_schema.is_none(),
            "a wiring selector alone proves no commit"
        );
        fixture.write("components/wms.json.in", &declaration);
        assert!(
            evidence(&fixture.attachments(), &attachment)
                .unwrap()
                .partial_schema
                .is_some()
        );
        for field in [
            "package-id",
            "component",
            "interface-version",
            "registered-operation",
        ] {
            let mut unrelated = declaration.clone();
            if field == "package-id" {
                unrelated["scope"][field] = json!("other");
            } else if field == "registered-operation" {
                unrelated["operations"]["wamn-wms:inventory/move@1.0.0"][field] = json!("other");
            } else {
                unrelated[field] = json!("other");
            }
            fixture.write("components/wms.json.in", &unrelated);
            assert!(
                evidence(&fixture.attachments(), &attachment)
                    .unwrap()
                    .partial_schema
                    .is_none(),
                "{field}"
            );
        }
        let mut without_response = wiring.clone();
        without_response.as_object_mut().unwrap().remove("response");
        fixture.write("wirings/composed.json", &without_response);
        let opaque = evidence(&fixture.attachments(), &attachment).unwrap();
        assert!(opaque.output_schema.is_none());
        assert!(opaque.partial_schema.is_none());
        let mut unreachable = wiring.clone();
        unreachable["nodes"]["unreachable"] = wiring["nodes"]["store"].clone();
        unreachable["response"]["node"] = json!("unreachable");
        fixture.write("wirings/composed.json", &unreachable);
        assert!(evidence(&fixture.attachments(), &attachment).is_err());
        fixture.write("wirings/composed.json", &wiring);
        fixture.write("components/wms.json.in", &declaration);
        fixture.write("components/duplicate.json.in", &declaration);
        assert!(
            evidence(&fixture.attachments(), &attachment)
                .unwrap()
                .partial_schema
                .is_none(),
            "ambiguous owners prove no commit"
        );
    }
}
