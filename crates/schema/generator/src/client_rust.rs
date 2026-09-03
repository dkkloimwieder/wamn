//! The Rust client emitter.
//!
//! Turns a [`ClientContractIr`] into Rust bindings: a request and result type
//! per operation, the field descriptors a control reads, the route the release
//! publishes, and one function that invokes it through `wamn-client`.
//!
//! # What is NOT emitted, and why
//!
//! No base URL and no host. Those are deployment facts a release does not
//! record — publication refuses an authored `route.host` and stamps one in at
//! mint — so a binding that carried one would be wrong the first time the same
//! release ran anywhere else. They stay construction-time config on the client
//! (ruling 3).
//!
//! The METHOD AND PATH TEMPLATE are emitted, because those are release facts:
//! the base package publishes its operation at `/purchase_order/get` and the
//! overlay publishes its own at `/acme/purchase_order/get`, so a client that
//! derived a path from a name would call the wrong one (ruling `wamn-10yt.5.8`).
//!
//! # Everything comes from the IR
//!
//! There is no hand-authored table here mapping operations to types, fields or
//! errors. A field added to a contract appears in the emitted types by
//! regeneration alone — that is the property this module exists to hold, and
//! the one its tests assert.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::client_ir::{ClientContractIr, FieldIr, ModelIr, OperationIr};
use crate::generate::GeneratedFile;
use crate::manifest::{rust_identifier, rust_type_identifier};

/// Why a client could not be emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientRustError {
    kind: ClientRustErrorKind,
    detail: String,
}

/// What went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientRustErrorKind {
    /// A contract type has no Rust spelling.
    UnknownType,
    /// A contract name cannot be a Rust identifier.
    UnnameableIdentifier,
}

impl ClientRustErrorKind {
    /// Stable wire code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownType => "unknown_type",
            Self::UnnameableIdentifier => "unnameable_identifier",
        }
    }
}

impl ClientRustError {
    fn new(kind: ClientRustErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// What went wrong.
    #[must_use]
    pub const fn kind(&self) -> ClientRustErrorKind {
        self.kind
    }
}

impl core::fmt::Display for ClientRustError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}: {}", self.kind.code(), self.detail)
    }
}

impl std::error::Error for ClientRustError {}

/// Emit one Rust module per model.
///
/// # Errors
///
/// [`ClientRustError`] naming the contract type or identifier that has no
/// Rust spelling. Refuses rather than emitting something approximate: a
/// binding that compiled but described the wrong shape would be found by a
/// caller at runtime, not by the build.
pub fn emit_rust_client(ir: &ClientContractIr) -> Result<Vec<GeneratedFile>, ClientRustError> {
    let mut files = BTreeMap::new();
    for model in &ir.models {
        files.insert(
            format!("generated/client/{}.rs", model.name),
            emit_model(&ir.package, model)?.into_bytes(),
        );
    }
    Ok(files
        .into_iter()
        .map(|(path, bytes)| GeneratedFile::new(path.into_boxed_str(), bytes.into_boxed_slice()))
        .collect())
}

fn emit_model(package: &str, model: &ModelIr) -> Result<String, ClientRustError> {
    let mut source = String::new();
    writeln!(
        source,
        "// @generated from the client-contract IR; do not edit."
    )
    .expect("writing to a String cannot fail");
    writeln!(source, "//!").expect("writing to a String cannot fail");
    writeln!(
        source,
        "//! `{}` operations of package `{package}`.",
        model.name
    )
    .expect("writing to a String cannot fail");
    writeln!(source).expect("writing to a String cannot fail");
    writeln!(
        source,
        "use wamn_client::{{ClientError, FieldDescriptor, RouteMetadata, WamnClient}};"
    )
    .expect("writing to a String cannot fail");
    writeln!(source).expect("writing to a String cannot fail");

    // The model's descriptor set: the union of what its operations return, so
    // a table over this model can render a column its `get` projects even when
    // the row came from `query`.
    write_descriptors(
        &mut source,
        &format!("{}_FIELDS", model.name.to_uppercase()),
        &format!("Every field the `{}` model projects.", model.name),
        &model.fields,
    );

    for operation in &model.operations {
        emit_operation(&mut source, model, operation)?;
    }

    while source.ends_with("\n\n") {
        source.pop();
    }
    Ok(source)
}

fn emit_operation(
    source: &mut String,
    model: &ModelIr,
    operation: &OperationIr,
) -> Result<(), ClientRustError> {
    let type_stem = format!(
        "{}{}",
        rust_type_identifier(&model.name),
        rust_type_identifier(&operation.name)
    );
    let constant_stem = format!(
        "{}_{}",
        model.name.to_uppercase(),
        operation.name.to_uppercase()
    );

    writeln!(source).expect("writing to a String cannot fail");
    writeln!(source, "/// Input for `{}`.", operation.operation).expect("write");
    write_struct(
        source,
        &format!("{type_stem}Request"),
        &operation.input_fields,
    )?;
    writeln!(source, "/// Result of `{}`.", operation.operation).expect("write");
    write_struct(
        source,
        &format!("{type_stem}Result"),
        &operation.result_fields,
    )?;

    write_descriptors(
        source,
        &format!("{constant_stem}_INPUT"),
        &format!("Input descriptors for `{}`.", operation.operation),
        &operation.input_fields,
    );
    write_descriptors(
        source,
        &format!("{constant_stem}_RESULT"),
        &format!("Result descriptors for `{}`.", operation.operation),
        &operation.result_fields,
    );

    writeln!(
        source,
        "/// The grant a caller presents to invoke `{}`.",
        operation.operation
    )
    .expect("write");
    writeln!(
        source,
        "pub const {constant_stem}_GRANT: &str = {:?};",
        operation.grant
    )
    .expect("write");
    writeln!(source).expect("write");

    writeln!(
        source,
        "/// Typed refusals `{}` declares.",
        operation.operation
    )
    .expect("write");
    writeln!(source, "pub const {constant_stem}_ERRORS: &[&str] = &[").expect("write");
    for error in &operation.errors {
        writeln!(source, "    {:?},", error.literal).expect("write");
    }
    writeln!(source, "];").expect("write");
    writeln!(source).expect("write");

    let Some(route) = &operation.route else {
        // Stated, not silently omitted: a caller reading the binding must
        // learn the operation exists and is not reachable over HTTP, rather
        // than find a function mysteriously missing.
        writeln!(
            source,
            "// `{}` is not published over HTTP by this release, so it has no route\n// and no invoke function. It remains listed for its types and descriptors.",
            operation.operation
        )
        .expect("write");
        writeln!(source).expect("write");
        return Ok(());
    };
    {
        {
            writeln!(
                source,
                "/// Where the release publishes `{}`.\n///\n/// Method and template only — the host and base URL are the client's\n/// deployment config, not this release's facts.",
                operation.operation
            )
            .expect("write");
            writeln!(source, "#[must_use]").expect("write");
            writeln!(
                source,
                "pub fn {}_route() -> RouteMetadata {{",
                operation.name
            )
            .expect("write");
            writeln!(source, "    RouteMetadata {{").expect("write");
            writeln!(source, "        method: {:?}.to_owned(),", route.method).expect("write");
            writeln!(source, "        template: {:?}.to_owned(),", route.template).expect("write");
            writeln!(source, "    }}").expect("write");
            writeln!(source, "}}").expect("write");
            writeln!(source).expect("write");
        }
    }

    writeln!(
        source,
        "/// Invoke `{}` through a bound client.\n///\n/// # Errors\n///\n/// [`ClientError`] for a transport failure, a refusal, or a response that\n/// does not match the operation's envelope.",
        operation.operation
    )
    .expect("write");
    let function = rust_identifier(&operation.name).ok_or_else(|| {
        ClientRustError::new(
            ClientRustErrorKind::UnnameableIdentifier,
            format!(
                "operation name {:?} is not a Rust identifier",
                operation.name
            ),
        )
    })?;
    writeln!(
        source,
        "pub async fn {function}(\n    client: &WamnClient,\n    items: &[serde_json::Value],\n) -> Result<Vec<wamn_client::ItemOutcome>, ClientError> {{"
    )
    .expect("write");
    writeln!(
        source,
        "    client\n        .invoke(&{}_route(), &std::collections::BTreeMap::new(), items)\n        .await\n}}",
        operation.name
    )
    .expect("write");
    writeln!(source).expect("write");
    Ok(())
}

fn write_struct(
    source: &mut String,
    name: &str,
    fields: &[FieldIr],
) -> Result<(), ClientRustError> {
    writeln!(source, "#[derive(Debug, Clone, PartialEq)]").expect("write");
    writeln!(source, "pub struct {name} {{").expect("write");
    for field in fields {
        // A dotted or indexed path is a position inside the envelope body, not
        // a struct member. Flattening `value.line[].quantity` into a name would
        // invent a shape the contract never declared, so those stay described
        // by their descriptors and are addressed through the envelope.
        if field.path.contains('.') || field.path.contains('[') {
            continue;
        }
        let member = rust_identifier(&field.path).ok_or_else(|| {
            ClientRustError::new(
                ClientRustErrorKind::UnnameableIdentifier,
                format!("field {:?} is not a Rust identifier", field.path),
            )
        })?;
        let ty = rust_type(&field.type_name)?;
        writeln!(
            source,
            "    /// `{}`{}",
            field.type_name,
            if field.nullable { ", optional" } else { "" }
        )
        .expect("write");
        if field.nullable {
            writeln!(source, "    pub {member}: Option<{ty}>,").expect("write");
        } else {
            writeln!(source, "    pub {member}: {ty},").expect("write");
        }
    }
    writeln!(source, "}}").expect("write");
    writeln!(source).expect("write");
    Ok(())
}

fn write_descriptors(source: &mut String, name: &str, doc: &str, fields: &[FieldIr]) {
    writeln!(source, "/// {doc}").expect("write");
    writeln!(source, "pub const {name}: &[FieldDescriptor] = &[").expect("write");
    for field in fields {
        writeln!(source, "    FieldDescriptor {{").expect("write");
        writeln!(source, "        path: {:?},", field.path).expect("write");
        writeln!(source, "        type_name: {:?},", field.type_name).expect("write");
        writeln!(source, "        nullable: {},", field.nullable).expect("write");
        if field.values.is_empty() {
            writeln!(source, "        values: &[],").expect("write");
        } else {
            writeln!(source, "        values: &[").expect("write");
            for value in &field.values {
                writeln!(source, "            {value:?},").expect("write");
            }
            writeln!(source, "        ],").expect("write");
        }
        writeln!(source, "    }},").expect("write");
    }
    writeln!(source, "];").expect("write");
    writeln!(source).expect("write");
}

/// The Rust spelling of one contract type.
///
/// The frozen `wamn:postgres` vocabulary is the authority — this reads it
/// through [`ColumnType`](wamn_schema_introspection::ir::ColumnType)'s own
/// wire literals rather than restating them, so a type added there cannot be
/// silently missed here. `string` is the one literal outside that vocabulary:
/// generated CRUD input contracts spell `request_id` that way.
fn rust_type(type_name: &str) -> Result<&'static str, ClientRustError> {
    use wamn_schema_introspection::ir::ColumnType;

    if type_name == "string" {
        return Ok("String");
    }
    let column: ColumnType =
        serde_json::from_value(serde_json::Value::String(type_name.to_owned())).map_err(|_| {
            ClientRustError::new(
                ClientRustErrorKind::UnknownType,
                format!("contract type {type_name:?} has no Rust spelling"),
            )
        })?;
    Ok(match column {
        ColumnType::Boolean => "bool",
        ColumnType::Int32 => "i32",
        ColumnType::Int64 => "i64",
        ColumnType::Float64 => "f64",
        ColumnType::Text => "String",
        ColumnType::Bytes => "Vec<u8>",
        ColumnType::Numeric => "rust_decimal::Decimal",
        ColumnType::Timestamptz => "chrono::DateTime<chrono::Utc>",
        ColumnType::Json => "serde_json::Value",
        ColumnType::Uuid => "uuid::Uuid",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The type map is a contract, and the compile gate cannot check it: a
    /// `uuid` emitted as `String` still compiles, because nothing in the
    /// scratch crate constructs a value. Mutation testing found exactly that
    /// hole, so the map is pinned here directly.
    #[test]
    fn every_contract_type_has_its_exact_rust_spelling() {
        for (contract, rust) in [
            ("boolean", "bool"),
            ("int32", "i32"),
            ("int64", "i64"),
            ("float64", "f64"),
            ("text", "String"),
            ("bytes", "Vec<u8>"),
            ("numeric", "rust_decimal::Decimal"),
            ("timestamptz", "chrono::DateTime<chrono::Utc>"),
            ("json", "serde_json::Value"),
            ("uuid", "uuid::Uuid"),
            // The one literal outside the frozen column vocabulary: generated
            // CRUD input contracts spell `request_id` this way.
            ("string", "String"),
        ] {
            assert_eq!(
                rust_type(contract).unwrap_or_else(|error| panic!("{contract}: {error}")),
                rust,
                "{contract}"
            );
        }
    }

    /// An unknown type REFUSES. Emitting an approximation would produce a
    /// binding that compiled and described the wrong shape, which a caller
    /// discovers at runtime rather than the build discovering it here.
    #[test]
    fn an_unknown_contract_type_refuses_by_name() {
        let refusal = rust_type("geography").expect_err("an unmapped type refuses");
        assert_eq!(refusal.kind(), ClientRustErrorKind::UnknownType);
        assert!(refusal.to_string().contains("geography"), "{refusal}");
    }

    /// Every type the shipped releases actually use is mapped. Arithmetic over
    /// the real corpus, so a vocabulary the packages adopt cannot reach the
    /// emitter unmapped without this failing.
    #[test]
    fn every_type_the_shipped_releases_use_is_mapped() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .and_then(std::path::Path::parent)
            .expect("repository root");
        for package in ["receiving", "client_acme_receiving"] {
            let ir = ClientContractIr::from_release(
                package,
                &root.join(format!("packages/{package}/generated/contracts")),
                &root.join(format!("packages/{package}/publication/attachments.json")),
            )
            .expect("projects");
            for model in &ir.models {
                for operation in &model.operations {
                    for field in operation
                        .input_fields
                        .iter()
                        .chain(&operation.result_fields)
                    {
                        rust_type(&field.type_name)
                            .unwrap_or_else(|error| panic!("{package} {}: {error}", field.path));
                    }
                }
            }
        }
    }
}
