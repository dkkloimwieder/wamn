//! Served-route evidence from exact publication declarations.

use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_catalog::{AttachmentTarget, ComponentDeclaration, ServingAttachment};

use crate::client_ir::{ClientIrError, ClientIrErrorType, read_authored};
use crate::operation_reference::AuthoredDocument;

#[derive(Debug, Default)]
pub(super) struct RouteEvidence {
    pub input_schema: Option<Value>,
    pub output_schema: Option<Value>,
    pub partial_schema: Option<Value>,
    pub terminal_operation: Option<String>,
    pub direct: bool,
}

/// Read the request schema and the route's or selected wiring's response owner.
pub(super) fn evidence(
    attachments: &Path,
    attachment: &ServingAttachment,
    generated: Option<&Value>,
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
    let AttachmentTarget::Route {
        component,
        operation,
    } = &attachment.target;
    result.direct = attachment.registered_operation.as_deref() == Some(operation.as_str());
    result.terminal_operation = Some(operation.clone());
    result.output_schema = component_schema(
        publication,
        generated,
        attachment,
        component,
        None,
        operation,
        false,
    )?;
    Ok(result)
}

/// Read one export's schema from the attachment package's component declarations.
///
/// A route names no interface version, so it passes `None` and matches any.
fn component_schema(
    publication: &Path,
    generated: Option<&Value>,
    attachment: &ServingAttachment,
    component: &str,
    interface_version: Option<&str>,
    export: &str,
    committed: bool,
) -> Result<Option<Value>, ClientIrError> {
    let mut selected = None;
    let mut matched = false;
    for path in declaration_paths(&publication.join("components"), true)? {
        // The package's generated operations belong to its declarations.
        let mut document = read_authored(&path, AuthoredDocument::Declaration)?;
        crate::route_schema::merge_operations(&mut document, generated)
            .map_err(|error| malformed(&path, error))?;
        let declaration: ComponentDeclaration = serde_json::from_value(document)
            .map_err(|error| malformed(&path, format!("invalid component declaration: {error}")))?;
        if declaration.scope.package_id != attachment.package_id
            || declaration.component != component
            || interface_version.is_some_and(|version| declaration.interface_version != version)
        {
            continue;
        }
        let Some(operation) = declaration.operations.get(export) else {
            continue;
        };
        if matched {
            return Ok(None);
        }
        matched = true;
        if committed {
            if operation.registered_operation.as_deref() != Some(export) {
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
        ClientIrErrorType::MalformedContract,
        format!("{}: {detail}", path.display()),
    )
}

fn unreadable(path: &Path, error: &std::io::Error) -> ClientIrError {
    ClientIrError::new(
        ClientIrErrorType::UnreadableProjection,
        format!("read {}: {error}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::{Value, json};
    use wamn_catalog::ServingAttachment;

    use super::evidence;

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
            std::fs::write(
                root.join("wamn.json"),
                r#"{"package": {"id": "platform_fixture", "version": "1.0.0"}}"#,
            )
            .expect("write the fixture package manifest");
            Self(root)
        }

        /// Write one authored document. An authored document names each
        /// package operation by reference, so the fixture package version
        /// leaves every sealed id the test built.
        fn write(&self, relative: &str, value: &Value) {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().expect("fixture parent"))
                .expect("create publication directory");
            let authored = serde_json::to_string(value)
                .expect("serialize fixture")
                .replace("@1.0.0\"", "\"");
            std::fs::write(path, authored).expect("write declaration");
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

    fn attachment(id: &str, _wiring_id: &str, operation: &str) -> ServingAttachment {
        serde_json::from_value(json!({
            "package-id": "platform_fixture",
            "type": "http",
            "component": "fixture",
            "operation": operation,
            "definition-hash": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "registered-operation": operation,
            "definition": {
                "route": {"path": format!("/{id}")},
                "input-schema": {"type": "array"}
            },
            "auth-policy": {"modes": ["pat"]}
        }))
        .expect("valid platform attachment")
    }

    #[test]
    fn direct_route_has_its_declared_response() {
        let attachment = attachment(
            "widget-archive-http",
            "widget-archive",
            "platform-fixture:widget/archive@1.0.0",
        );
        let fixture = Publication::new();
        fixture.write("components/fixture.json.in", &json!({
            "scope": {"tenant-id": "tenant-a", "package-id": "platform_fixture"},
            "component": "fixture", "interface-version": "1.0.0",
            "operations": {"platform-fixture:widget/archive@1.0.0": {
                "registered-operation": "platform-fixture:widget/archive@1.0.0",
                "input-ports": [], "output-ports": [{"name": "main", "schema": {"type": "array"}}], "parameters": []
            }}, "connections": []
        }));
        let result = evidence(&fixture.attachments(), &attachment, None)
            .expect("read direct publication evidence");
        assert!(result.direct);
        assert_eq!(result.terminal_operation, attachment.registered_operation);
        assert_eq!(
            result.input_schema,
            Some(attachment.definition["input-schema"].clone())
        );
        assert_eq!(result.output_schema, Some(json!({"type": "array"})));
    }
}
