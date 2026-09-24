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
    let path = package_root.join(checked(reference)?);
    let subject = path.display().to_string();
    let bytes = std::fs::read(&path).map_err(|error| {
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

/// Read a package's `publication/attachments.json` with every reference resolved.
///
/// # Errors
///
/// [`RouteSchemaError`] when the document is unreadable or not an attachment
/// map, or a reference fails.
pub fn read_package_attachments(
    package_root: &Path,
) -> Result<BTreeMap<String, ServingAttachment>, RouteSchemaError> {
    let path = package_root.join("publication/attachments.json");
    let subject = path.display().to_string();
    let bytes = std::fs::read(&path).map_err(|error| {
        RouteSchemaError::with_source(RouteSchemaErrorKind::Read, &subject, error)
    })?;
    let mut attachments = serde_json::from_slice(&bytes).map_err(|error| {
        RouteSchemaError::with_source(RouteSchemaErrorKind::Parse, &subject, error)
    })?;
    resolve_attachments(&mut attachments, &mut |reference| {
        read_from_package(package_root, reference)
    })?;
    Ok(attachments)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

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
