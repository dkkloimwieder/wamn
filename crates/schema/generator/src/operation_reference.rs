//! Operation references in the authored publication files.
//!
//! An authored route entry, component declaration or wiring names an operation
//! by its reference, `<package>:<interface>/<operation>`. The version is
//! authored once, as `package.version` in `wamn.json`. Every reader resolves
//! the references of an authored document with this module before it uses the
//! document, so no reader sees a reference and every reader sees the same
//! sealed ids (docs/plan/operation-ids.md).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::{GenerateError, GenerateErrorKind, OperationOwners, resolve_operation_reference};

/// The package manifest file at a package root.
pub const PACKAGE_MANIFEST: &str = "wamn.json";

/// The prefix of a platform interface. A document names one with its WIT
/// version, and it is not an operation of a package.
const PLATFORM_PREFIX: &str = "wamn:";

/// Read the operation owners of the package that holds an authored document,
/// from the nearest `wamn.json` among the ancestors of the document, and the
/// package root that holds it.
///
/// # Errors
///
/// [`GenerateError`] when no ancestor holds a `wamn.json`, or it does not parse.
pub fn package_owners_of(document: &Path) -> Result<(PathBuf, OperationOwners), GenerateError> {
    let root = document
        .ancestors()
        .skip(1)
        .find(|directory| directory.join(PACKAGE_MANIFEST).is_file())
        .ok_or_else(|| {
            GenerateError::new(
                GenerateErrorKind::InvalidManifest,
                format!(
                    "{} is outside a package: no ancestor holds {PACKAGE_MANIFEST}",
                    document.display()
                ),
            )
        })?;
    let path = root.join(PACKAGE_MANIFEST);
    let bytes = std::fs::read(&path).map_err(|error| {
        GenerateError::with_source(
            GenerateErrorKind::InvalidManifest,
            format!("read {}", path.display()),
            error,
        )
    })?;
    Ok((root.to_owned(), OperationOwners::from_slice(&bytes)?))
}

/// The kind of one authored publication file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthoredDocument {
    /// `publication/attachments.json`.
    Attachments,
    /// `publication/components/<component>.json.in`.
    Declaration,
    /// `publication/wirings/<wiring>.json`.
    Wiring,
}

/// Read one authored publication file, with its references resolved against
/// the `wamn.json` of the package that holds it.
///
/// # Errors
///
/// [`GenerateError`] when the file is unreadable or not JSON, when it is
/// outside a package, or when a reference refuses.
pub fn read_authored_document(path: &Path, kind: AuthoredDocument) -> Result<Value, GenerateError> {
    let bytes = std::fs::read(path).map_err(|error| {
        GenerateError::with_source(
            GenerateErrorKind::InvalidManifest,
            format!("read {}", path.display()),
            error,
        )
    })?;
    let mut document: Value = serde_json::from_slice(&bytes).map_err(|error| {
        GenerateError::with_source(
            GenerateErrorKind::InvalidManifest,
            format!("parse {}", path.display()),
            error,
        )
    })?;
    let (_, owners) = package_owners_of(path)?;
    resolve_authored_document(&mut document, kind, &owners).map_err(|error| {
        GenerateError::new(
            error.kind(),
            format!("{}: {}", path.display(), error.context()),
        )
    })?;
    Ok(document)
}

/// Resolve the references of one authored publication document.
///
/// # Errors
///
/// [`GenerateError`] naming the member whose reference refuses.
pub fn resolve_authored_document(
    document: &mut Value,
    kind: AuthoredDocument,
    owners: &OperationOwners,
) -> Result<(), GenerateError> {
    match kind {
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
/// operation key, and its `registered-operation`, `pre-commit` and
/// `dependencies[].operation` and `participant`. The declaration takes
/// `scope.package-version` from the manifest.
///
/// # Errors
///
/// [`GenerateError`] naming the member whose reference refuses, or a template
/// that authors `scope.package-version`.
pub fn resolve_declaration_document(
    document: &mut Value,
    owners: &OperationOwners,
) -> Result<(), GenerateError> {
    if let Some(scope) = document.get_mut("scope").and_then(Value::as_object_mut) {
        if scope.contains_key("package-version") {
            return Err(GenerateError::new(
                GenerateErrorKind::InvalidIdentity,
                "a declaration must not author scope.package-version; \
                 the version is authored once, as package.version in wamn.json",
            ));
        }
        scope.insert("package-version".to_owned(), json!(owners.package.version));
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
        for member in ["registered-operation", "pre-commit"] {
            resolve_member(&mut operation, member, owners, &context)?;
        }
        if let Some(dependencies) = operation
            .get_mut("dependencies")
            .and_then(Value::as_array_mut)
        {
            for dependency in dependencies {
                for member in ["operation", "participant"] {
                    resolve_member(dependency, member, owners, &context)?;
                }
            }
        }
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
            GenerateErrorKind::InvalidIdentity,
            format!("{context}: {member} must be an operation reference"),
        ));
    };
    let sealed = resolve_reference(owners, reference).map_err(|error| {
        GenerateError::new(
            GenerateErrorKind::InvalidIdentity,
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

    /// Each kind resolves its references, and a platform interface stays.
    #[test]
    fn every_authored_kind_resolves_its_references() {
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
            "scope": {"tenant-id": "t", "package-id": "acme"},
            "operations": {"acme:order/record": {
                "registered-operation": "acme:order/record",
                "dependencies": [{
                    "operation": "base-app:order/record", "participant": "acme:order/check"
                }]
            }}
        });
        resolve_declaration_document(&mut declaration, &owners()).unwrap();
        assert_eq!(declaration["scope"]["package-version"], "3.0.0");
        let operation = &declaration["operations"]["acme:order/record@3.0.0"];
        assert_eq!(operation["registered-operation"], "acme:order/record@3.0.0");
        let dependency = &operation["dependencies"][0];
        assert_eq!(dependency["operation"], "base-app:order/record@1.0.0");
        assert_eq!(dependency["participant"], "acme:order/check@3.0.0");

        let mut wiring = json!({"nodes": {
            "a": {"operation": "acme:order/get"},
            "b": {"operation": "wamn:node/handler@0.1.0"}
        }});
        resolve_wiring_document(&mut wiring, &owners()).unwrap();
        assert_eq!(wiring["nodes"]["a"]["operation"], "acme:order/get@3.0.0");
        assert_eq!(wiring["nodes"]["b"]["operation"], "wamn:node/handler@0.1.0");
    }

    /// An authored version refuses, in an id or in the declaration scope.
    #[test]
    fn an_authored_version_refuses() {
        let mut attachments = json!({"a": {"operation": "acme:order/get@3.0.0"}});
        let error = resolve_attachments_document(&mut attachments, &owners()).unwrap_err();
        assert!(
            error.context().contains("must not carry a version"),
            "{error}"
        );

        let mut declaration = json!({"scope": {"package-id": "acme", "package-version": "3.0.0"}});
        let error = resolve_declaration_document(&mut declaration, &owners()).unwrap_err();
        assert!(error.context().contains("scope.package-version"), "{error}");
    }
}
