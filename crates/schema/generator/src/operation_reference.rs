//! Resolve authored operation references once during publication compilation.

use crate::manifest::{OperationOwners, resolve_operation_reference};
use crate::{ManifestError as GenerateError, ManifestErrorType as GenerateErrorType};
use serde_json::{Map, Value, json};
const PLATFORM_PREFIX: &str = "wamn:";

/// The type of one authored publication file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthoredDocument {
    /// `publication/attachments.json`.
    Attachments,
    /// `publication/components/<component>.json.in`.
    Declaration,
    /// `publication/wirings/<wiring>.json`.
    Wiring,
}

/// Read an authored `publication/attachments.json` as its map of attachment id
/// to entry, with references unresolved. A missing file means that the package
/// authors no attachments, so it reads as an empty map. Every reader of the
/// file reads it here.
///
/// # Errors
///
/// [`GenerateError`] when the file exists and is unreadable, is not JSON, or
/// is not a JSON object.
pub fn read_attachments_document(
    path: &std::path::Path,
) -> Result<Map<String, Value>, GenerateError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => {
            return Err(GenerateError::with_source(
                GenerateErrorType::InvalidManifest,
                format!("read {}", path.display()),
                error,
            ));
        }
    };
    serde_json::from_slice(&bytes).map_err(|error| {
        GenerateError::with_source(
            GenerateErrorType::InvalidManifest,
            format!("parse {}", path.display()),
            error,
        )
    })
}

/// Resolve the references of one authored publication document.
///
/// # Errors
///
/// [`GenerateError`] naming the member whose reference refuses.
pub fn resolve_authored_document(
    document: &mut Value,
    type_: AuthoredDocument,
    owners: &OperationOwners,
) -> Result<(), GenerateError> {
    match type_ {
        AuthoredDocument::Attachments => resolve_attachments_document(document, owners),
        AuthoredDocument::Declaration => resolve_declaration_document(document, owners),
        AuthoredDocument::Wiring => resolve_wiring_document(document, owners),
    }
}

/// Resolve every reference of an authored `publication/attachments.json`: the
/// `operation` and `registered-operation` of each entry.
///
/// # Errors
///
/// [`GenerateError`] naming the member whose reference refuses.
pub fn resolve_attachments_document(
    document: &mut Value,
    owners: &OperationOwners,
) -> Result<(), GenerateError> {
    let Some(entries) = document.as_object_mut() else {
        return Ok(());
    };
    for (attachment_id, entry) in entries {
        resolve_attachment_entry(attachment_id, entry, owners)?;
    }
    Ok(())
}

/// Resolve the `operation` and `registered-operation` of one authored route
/// entry, against the owners of the package the entry names.
///
/// # Errors
///
/// [`GenerateError`] naming the member whose reference refuses.
pub fn resolve_attachment_entry(
    attachment_id: &str,
    entry: &mut Value,
    owners: &OperationOwners,
) -> Result<(), GenerateError> {
    for member in ["operation", "registered-operation"] {
        resolve_member(
            entry,
            member,
            owners,
            &format!("attachment {attachment_id:?}"),
        )?;
    }
    Ok(())
}

/// Resolve every reference of an authored component declaration: each
/// operation key and its `registered-operation`. The declaration takes
/// `target.package-version` from the manifest.
///
/// # Errors
///
/// [`GenerateError`] naming the member whose reference refuses, or a template
/// that authors `target.package-version`.
pub fn resolve_declaration_document(
    document: &mut Value,
    owners: &OperationOwners,
) -> Result<(), GenerateError> {
    if let Some(target) = document.get_mut("target").and_then(Value::as_object_mut) {
        if target.contains_key("package-version") {
            return Err(GenerateError::new(
                GenerateErrorType::InvalidIdentity,
                "a declaration must not author target.package-version; \
                 the version is authored once, as package.version in wamn.json",
            ));
        }
        target.insert("package-version".to_owned(), json!(owners.package.version));
    }
    let Some(operations) = document
        .get_mut("operations")
        .and_then(Value::as_object_mut)
    else {
        return Ok(());
    };
    let authored = std::mem::take(operations);
    for (reference, mut operation) in authored {
        let context = format!("declaration operation {reference:?}");
        resolve_member(&mut operation, "registered-operation", owners, &context)?;
        operations.insert(resolve_reference(owners, &reference)?, operation);
    }
    Ok(())
}

/// Resolve every package operation that an authored wiring names in
/// `nodes.*.operation`.
///
/// # Errors
///
/// [`GenerateError`] naming the node whose reference refuses.
pub fn resolve_wiring_document(
    document: &mut Value,
    owners: &OperationOwners,
) -> Result<(), GenerateError> {
    let Some(nodes) = document.get_mut("nodes").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    for (node_id, node) in nodes {
        resolve_member(
            node,
            "operation",
            owners,
            &format!("wiring node {node_id:?}"),
        )?;
    }
    Ok(())
}

fn resolve_member(
    value: &mut Value,
    member: &str,
    owners: &OperationOwners,
    context: &str,
) -> Result<(), GenerateError> {
    let Some(slot) = value.get_mut(member) else {
        return Ok(());
    };
    let Some(reference) = slot.as_str() else {
        return Err(GenerateError::new(
            GenerateErrorType::InvalidIdentity,
            format!("{context}: {member} must be an operation reference"),
        ));
    };
    let sealed = resolve_reference(owners, reference).map_err(|error| {
        GenerateError::new(
            GenerateErrorType::InvalidIdentity,
            format!("{context}: {member}: {}", error.context()),
        )
    })?;
    *slot = Value::String(sealed);
    Ok(())
}

/// The sealed id of one reference. A platform interface, such as
/// `wamn:node/handler@0.1.0`, is not an operation of a package and stays as
/// written, with its WIT version.
fn resolve_reference(owners: &OperationOwners, reference: &str) -> Result<String, GenerateError> {
    if reference.starts_with(PLATFORM_PREFIX) {
        return Ok(reference.to_owned());
    }
    resolve_operation_reference(owners, reference)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn owners() -> OperationOwners {
        serde_json::from_value(json!({
            "package": {"id": "acme", "version": "3.0.0"},
            "base_dependencies": {"base": {
                "package": "base_app", "version": "1.0.0",
                "digest": "sha256:0", "operations": ["order.record"]
            }}
        }))
        .expect("owners")
    }

    /// Each type resolves its references, and a platform interface stays.
    #[test]
    fn every_authored_type_resolves_its_references() {
        let mut attachments = json!({"a": {
            "operation": "acme:order/get", "registered-operation": "acme:order/get"
        }});
        resolve_attachments_document(&mut attachments, &owners()).unwrap();
        assert_eq!(attachments["a"]["operation"], "acme:order/get@3.0.0");
        assert_eq!(
            attachments["a"]["registered-operation"],
            "acme:order/get@3.0.0"
        );

        let mut declaration = json!({
            "target": {"project_env_key": "t", "package-id": "acme"},
            "operations": {"acme:order/record": {
                "registered-operation": "acme:order/record"
            }}
        });
        resolve_declaration_document(&mut declaration, &owners()).unwrap();
        assert_eq!(declaration["target"]["package-version"], "3.0.0");
        let operation = &declaration["operations"]["acme:order/record@3.0.0"];
        assert_eq!(operation["registered-operation"], "acme:order/record@3.0.0");

        let mut wiring = json!({"nodes": {
            "a": {"operation": "acme:order/get"},
            "b": {"operation": "wamn:node/handler@0.1.0"}
        }});
        resolve_wiring_document(&mut wiring, &owners()).unwrap();
        assert_eq!(wiring["nodes"]["a"]["operation"], "acme:order/get@3.0.0");
        assert_eq!(wiring["nodes"]["b"]["operation"], "wamn:node/handler@0.1.0");
    }

    /// An authored version refuses, in an id or in the declaration target.
    #[test]
    fn an_authored_version_refuses() {
        let mut attachments = json!({"a": {"operation": "acme:order/get@3.0.0"}});
        let error = resolve_attachments_document(&mut attachments, &owners()).unwrap_err();
        assert!(
            error.context().contains("must not carry a version"),
            "{error}"
        );

        let mut declaration = json!({"target": {"package-id": "acme", "package-version": "3.0.0"}});
        assert!(resolve_declaration_document(&mut declaration, &owners()).is_err());
    }
}
