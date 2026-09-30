//! Generated route input schemas and the references that name them.
//!
//! Generation derives the route input schema of every generated CRUD operation
//! from its input contract and writes it under [`GENERATED_ROUTES`]. A package
//! names that file, and never copies it, in two places: the route's
//! `input-schema` in `publication/attachments.json`, and the operation's input
//! port in its component declaration. Both are written as
//! `{"$ref": "generated/routes/<model>/<action>.json"}`, relative to the package
//! root (wamn-4omo).
//!
//! Every reader that serves or checks a schema resolves the reference first:
//! publication, component admission and the client generator. A reference that
//! reaches the router unresolved cannot compile, so the route refuses every
//! call rather than admitting any body.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::Path;

use serde_json::Value;
use wamn_catalog::{DefinitionHash, ServingAttachment};

/// The package directory generation writes route input schemas into.
pub const GENERATED_ROUTES: &str = "generated/routes/";

/// The route entries generation writes for the generated operations.
pub const GENERATED_ATTACHMENTS: &str = "generated/publication/attachments.json";

/// The declaration entries generation writes for the generated operations,
/// keyed by component and then by operation.
pub const GENERATED_COMPONENT_OPERATIONS: &str = "generated/publication/component-operations.json";

/// The one member of a schema value that names a generated schema.
const REFERENCE: &str = "$ref";

/// Stable category of a route schema failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteSchemaErrorKind {
    /// A reference names a file outside the generated route schemas.
    Reference,
    /// The named file could not be read.
    Read,
    /// The named file or the attachment document is not JSON of its shape.
    Parse,
    /// An authored definition hash does not identify its authored definition.
    DefinitionHash,
    /// An authored document repeats an entry that generation writes.
    Duplicate,
}

/// Contextual failure to resolve a generated route schema.
#[derive(Debug)]
pub struct RouteSchemaError {
    kind: RouteSchemaErrorKind,
    subject: Box<str>,
    detail: Box<str>,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl RouteSchemaError {
    fn new(kind: RouteSchemaErrorKind, subject: &str, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            subject: subject.into(),
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        kind: RouteSchemaErrorKind,
        subject: &str,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            source: Some(Box::new(source)),
            ..Self::new(kind, subject, "")
        }
    }

    /// A reference to a schema that generation did not produce.
    pub fn unknown(reference: &str) -> Self {
        Self::new(
            RouteSchemaErrorKind::Reference,
            reference,
            "names no route schema that generation produced",
        )
    }

    /// Stable failure category.
    pub const fn kind(&self) -> RouteSchemaErrorKind {
        self.kind
    }
}

impl fmt::Display for RouteSchemaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            RouteSchemaErrorKind::Read => write!(formatter, "read {}", self.subject),
            RouteSchemaErrorKind::Parse if self.detail.is_empty() => {
                write!(formatter, "parse {}", self.subject)
            }
            _ => write!(formatter, "{} {}", self.subject, self.detail),
        }
    }
}

impl Error for RouteSchemaError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

/// The generated schema one schema value names, when it names one.
///
/// Only a value that is exactly `{"$ref": "<path>"}` names one. Any other
/// value is an authored schema and stays as written.
pub fn reference(schema: &Value) -> Option<&str> {
    let members = schema.as_object()?;
    if members.len() != 1 {
        return None;
    }
    members.get(REFERENCE)?.as_str()
}

/// Refuse a reference outside the package's generated route schemas.
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "a reference names an exact generated path"
)]
fn checked(reference: &str) -> Result<&str, RouteSchemaError> {
    let inside = reference
        .strip_prefix(GENERATED_ROUTES)
        .is_some_and(|rest| {
            rest.ends_with(".json")
                && rest
                    .split('/')
                    .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        })
        && !reference.contains('\\');
    if inside {
        Ok(reference)
    } else {
        Err(RouteSchemaError::new(
            RouteSchemaErrorKind::Reference,
            reference,
            format!("must name a {GENERATED_ROUTES}<model>/<action>.json file of its package"),
        ))
    }
}

/// Read one generated schema from a package on disk.
///
/// # Errors
///
/// [`RouteSchemaError`] when the reference leaves the generated route schemas,
/// or the file is unreadable or not JSON.
pub fn read_from_package(package_root: &Path, reference: &str) -> Result<Value, RouteSchemaError> {
    read_json(&package_root.join(checked(reference)?))
}

fn read_json(path: &Path) -> Result<Value, RouteSchemaError> {
    let subject = path.display().to_string();
    let bytes = std::fs::read(path).map_err(|error| {
        RouteSchemaError::with_source(RouteSchemaErrorKind::Read, &subject, error)
    })?;
    parse(&subject, &bytes)
}

/// Parse the bytes of one generated schema.
///
/// # Errors
///
/// [`RouteSchemaError`] when the bytes are not JSON.
pub fn parse(subject: &str, bytes: &[u8]) -> Result<Value, RouteSchemaError> {
    serde_json::from_slice(bytes)
        .map_err(|error| RouteSchemaError::with_source(RouteSchemaErrorKind::Parse, subject, error))
}

/// Replace one schema value by the generated schema it names.
fn resolve(
    schema: &mut Value,
    read: &mut dyn FnMut(&str) -> Result<Value, RouteSchemaError>,
) -> Result<(), RouteSchemaError> {
    if let Some(reference) = reference(schema) {
        *schema = read(checked(reference)?)?;
    }
    Ok(())
}

/// Resolve the input schema reference of every attachment in one map.
///
/// # Errors
///
/// [`RouteSchemaError`] naming the attachment whose hash or reference fails.
pub fn resolve_attachments(
    attachments: &mut BTreeMap<String, ServingAttachment>,
    read: &mut dyn FnMut(&str) -> Result<Value, RouteSchemaError>,
) -> Result<(), RouteSchemaError> {
    for (attachment_id, attachment) in attachments {
        resolve_attachment(attachment_id, attachment, read)?;
    }
    Ok(())
}

/// Resolve the input schema reference of one attachment, when it names one.
///
/// The authored `definition-hash` identifies the authored definition, which
/// names the file. The hash is checked against it, and then derived again over
/// the definition that carries the schema, so a contract change moves the
/// served hash with no edit to the attachment document.
///
/// # Errors
///
/// [`RouteSchemaError`] naming the attachment whose hash or reference fails.
pub fn resolve_attachment(
    attachment_id: &str,
    attachment: &mut ServingAttachment,
    read: &mut dyn FnMut(&str) -> Result<Value, RouteSchemaError>,
) -> Result<(), RouteSchemaError> {
    if !names_generated_schema(attachment) {
        return Ok(());
    }
    let authored = wamn_execution_contract::canonical_json_sha256(&attachment.definition);
    if attachment.definition_hash.as_str() != authored {
        return Err(RouteSchemaError::new(
            RouteSchemaErrorKind::DefinitionHash,
            &format!("attachment {attachment_id:?}"),
            format!(
                "definition-hash {} differs from canonical definition hash {authored}",
                attachment.definition_hash.as_str()
            ),
        ));
    }
    resolve(&mut attachment.definition["input-schema"], read)?;
    attachment.definition_hash = DefinitionHash::parse(
        wamn_execution_contract::canonical_json_sha256(&attachment.definition),
    )
    .expect("the shared canonicalizer emits a valid definition hash");
    Ok(())
}

/// Whether an attachment's input schema names a generated schema.
pub fn names_generated_schema(attachment: &ServingAttachment) -> bool {
    attachment
        .definition
        .get("input-schema")
        .and_then(reference)
        .is_some()
}

/// Resolve every input port reference in one component declaration document.
///
/// # Errors
///
/// [`RouteSchemaError`] naming the reference that fails.
pub fn resolve_declaration(
    document: &mut Value,
    read: &mut dyn FnMut(&str) -> Result<Value, RouteSchemaError>,
) -> Result<(), RouteSchemaError> {
    let operations = document
        .get_mut("operations")
        .and_then(Value::as_object_mut)
        .into_iter()
        .flat_map(|operations| operations.values_mut());
    for operation in operations {
        let ports = operation
            .get_mut("input-ports")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten();
        for port in ports {
            if let Some(schema) = port.get_mut("schema") {
                resolve(schema, read)?;
            }
        }
    }
    Ok(())
}

/// The package root above an authored document at `<root>/publication/<file>`.
pub fn package_root_of(authored: &Path) -> &Path {
    authored
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new(""))
}

/// Read one generated publication file of a package on disk, or `None` when
/// generation wrote none because the package has no generated operation.
///
/// # Errors
///
/// [`RouteSchemaError`] when the file exists and is unreadable or not JSON.
pub fn read_generated_publication(
    package_root: &Path,
    file: &str,
) -> Result<Option<Value>, RouteSchemaError> {
    let path = package_root.join(file);
    if path.exists() {
        read_json(&path).map(Some)
    } else {
        Ok(None)
    }
}

/// Parse a generated attachment document. `None` is an empty map.
///
/// # Errors
///
/// [`RouteSchemaError`] when the value is not an attachment map.
pub fn generated_attachments(
    document: Option<Value>,
) -> Result<BTreeMap<String, ServingAttachment>, RouteSchemaError> {
    document.map_or_else(
        || Ok(BTreeMap::new()),
        |document| {
            serde_json::from_value(document).map_err(|error| {
                RouteSchemaError::with_source(
                    RouteSchemaErrorKind::Parse,
                    GENERATED_ATTACHMENTS,
                    error,
                )
            })
        },
    )
}

/// Add the generated attachments to the authored ones.
///
/// # Errors
///
/// [`RouteSchemaError`] naming an attachment id that both carry.
pub fn merge_attachments(
    authored: &mut BTreeMap<String, ServingAttachment>,
    generated: BTreeMap<String, ServingAttachment>,
) -> Result<(), RouteSchemaError> {
    for (attachment_id, attachment) in generated {
        if authored.contains_key(&attachment_id) {
            return Err(duplicate(&format!("attachment {attachment_id:?}")));
        }
        authored.insert(attachment_id, attachment);
    }
    Ok(())
}

/// Add the generated operation entries of a declaration's component to one
/// authored component declaration document.
///
/// # Errors
///
/// [`RouteSchemaError`] naming an operation that both carry.
pub fn merge_operations(
    declaration: &mut Value,
    generated: Option<&Value>,
) -> Result<(), RouteSchemaError> {
    let component = declaration.get("component").and_then(Value::as_str);
    let Some(entries) = generated
        .zip(component)
        .and_then(|(generated, component)| generated.get(component))
        .and_then(Value::as_object)
        .cloned()
    else {
        return Ok(());
    };
    let Some(operations) = declaration
        .get_mut("operations")
        .and_then(Value::as_object_mut)
    else {
        return Err(RouteSchemaError::new(
            RouteSchemaErrorKind::Parse,
            "component declaration",
            "has no operations object",
        ));
    };
    for (operation, entry) in entries {
        if operations.contains_key(&operation) {
            return Err(duplicate(&format!("operation {operation:?}")));
        }
        operations.insert(operation, entry);
    }
    Ok(())
}

fn duplicate(subject: &str) -> RouteSchemaError {
    RouteSchemaError::new(
        RouteSchemaErrorKind::Duplicate,
        subject,
        "is generated; remove it from the authored document",
    )
}

/// Read a package's `publication/attachments.json` and its generated route
/// entries, with every reference resolved.
///
/// # Errors
///
/// [`RouteSchemaError`] when a document is unreadable or not an attachment
/// map, an attachment id appears in both, or a reference fails.
pub fn read_package_attachments(
    package_root: &Path,
) -> Result<BTreeMap<String, ServingAttachment>, RouteSchemaError> {
    let path = package_root.join("publication/attachments.json");
    let subject = path.display().to_string();
    let document = crate::operation_reference::read_authored_document(
        &path,
        crate::operation_reference::AuthoredDocument::Attachments,
    )
    .map_err(|error| RouteSchemaError::with_source(RouteSchemaErrorKind::Read, &subject, error))?;
    let mut attachments = serde_json::from_value(document).map_err(|error| {
        RouteSchemaError::with_source(RouteSchemaErrorKind::Parse, &subject, error)
    })?;
    merge_attachments(
        &mut attachments,
        generated_attachments(read_generated_publication(
            package_root,
            GENERATED_ATTACHMENTS,
        )?)?,
    )?;
    resolve_attachments(&mut attachments, &mut |reference| {
        read_from_package(package_root, reference)
    })?;
    Ok(attachments)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// An authored entry that generation also writes refuses by its name.
    #[test]
    fn an_authored_copy_of_a_generated_entry_refuses() {
        let attachment: ServingAttachment = serde_json::from_value(json!({
            "type": "http", "package-id": "p", "component": "c",
            "operation": "p:m/get@1.0.0", "registered-operation": "p:m/get@1.0.0",
            "definition-hash": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "definition": {}, "auth-policy": {"modes": ["pat"]}
        }))
        .expect("an attachment");
        let mut authored = BTreeMap::from([("m-get-http".to_owned(), attachment.clone())]);
        let error = merge_attachments(
            &mut authored,
            BTreeMap::from([("m-get-http".to_owned(), attachment)]),
        )
        .expect_err("a repeated attachment id refuses");
        assert_eq!(error.kind(), RouteSchemaErrorKind::Duplicate);
        assert!(error.to_string().contains("m-get-http"), "{error}");

        let mut declaration = json!({"component": "c", "operations": {"p:m/get@1.0.0": {}}});
        let generated = json!({"c": {"p:m/get@1.0.0": {}}});
        let error = merge_operations(&mut declaration, Some(&generated))
            .expect_err("a repeated operation refuses");
        assert_eq!(error.kind(), RouteSchemaErrorKind::Duplicate);
        assert!(error.to_string().contains("p:m/get@1.0.0"), "{error}");
    }

    #[test]
    fn a_reference_names_only_a_generated_route_schema() {
        assert_eq!(
            reference(&json!({"$ref": "generated/routes/pallet/create.json"})),
            Some("generated/routes/pallet/create.json")
        );
        assert_eq!(reference(&json!({"$ref": "x", "type": "array"})), None);
        assert_eq!(reference(&json!({"type": "array"})), None);
        for escaping in [
            "generated/routes/../wamn.json",
            "generated/contracts/pallet/create.input.json",
            "/generated/routes/pallet/create.json",
            "generated/routes/pallet/create.sql",
        ] {
            assert_eq!(
                checked(escaping).expect_err(escaping).kind(),
                RouteSchemaErrorKind::Reference
            );
        }
    }
}
