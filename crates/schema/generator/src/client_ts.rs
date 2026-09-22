//! The TypeScript client emitter.
//!
//! Turns a [`ClientContractIr`](crate::client_ir::ClientContractIr) into
//! TypeScript bindings: a request and result interface per operation, the route
//! the release publishes, and one function that sends it through a transport
//! the application supplies.
//!
//! # What is NOT emitted, and why
//!
//! No base URL and no host, for the reason [`crate::client_rust`] states: a
//! release does not record a deployment fact. No framework import and no
//! component. The transport interface is declared here and implemented outside
//! the generator.
//!
//! No classification of a response. The four outcomes of one intent come from
//! the response status, the raw body, the result class, the result fields and
//! the partial completion contract together, which
//! `crates/client/tui/src/submission.rs` owns for the terminal. A transport
//! returns an [`Outcome`](self) that it classified, so the bindings hold no
//! second copy of that reducer.
//!
//! # Names
//!
//! The contract is snake_case and the wire keeps it. TypeScript members are
//! camelCase. [`to_camel`] decides every member name here, at emission, and it
//! is the only place that decides one. The rule is reversible: an underscore
//! becomes a capital only when a lowercase letter follows it, so `line_1`
//! keeps its underscore.
//!
//! The emitted module holds no name rule. Each operation carries a field map
//! that states, for one declared shape, the wire key and the member beside it.
//! The emitted `fromWire` and `toWire` rename the keys that a map declares and
//! leave every other key exactly as it arrived, so the inside of a `json`
//! value is never renamed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::client_ir::{
    ClientContractIr, FieldIr, ModelIr, OperationIr, ReplayIr, SORT_DIRECTION_INPUT,
    SORT_FIELD_INPUT, SortIr,
};
use crate::generate::GeneratedFile;

/// Why a TypeScript client could not be emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientTsError {
    kind: ClientTsErrorKind,
    detail: String,
}

/// What went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTsErrorKind {
    /// A contract type has no TypeScript spelling.
    UnknownType,
    /// A contract name cannot be a TypeScript identifier.
    UnnameableIdentifier,
    /// A contract name does not reverse through the member rule.
    IrreversibleName,
    /// Two contract names take the same TypeScript name.
    NameCollision,
}

impl ClientTsErrorKind {
    /// Stable wire code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownType => "unknown_type",
            Self::UnnameableIdentifier => "unnameable_identifier",
            Self::IrreversibleName => "irreversible_name",
            Self::NameCollision => "name_collision",
        }
    }
}

impl ClientTsError {
    pub(crate) fn new(kind: ClientTsErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// What went wrong.
    #[must_use]
    pub const fn kind(&self) -> ClientTsErrorKind {
        self.kind
    }
}

impl core::fmt::Display for ClientTsError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}: {}", self.kind.code(), self.detail)
    }
}

impl std::error::Error for ClientTsError {}

/// The TypeScript spelling of one contract type.
///
/// The frozen `wamn:postgres` vocabulary is the authority, read through
/// [`ColumnType`](wamn_schema_introspection::ir::ColumnType)'s own wire
/// literals rather than restated, so a type added there cannot be silently
/// missed here. `string` is the one literal outside that vocabulary: generated
/// CRUD input contracts spell `request_id` that way.
///
/// `int64` and `numeric` are strings, because that is what the wire carries.
/// `canonical_numeric` in `crates/client/core/src/request.rs` writes a decimal
/// string, and `canonical_integer` writes an `int64` as a string when the route
/// schema states no type. A TypeScript number loses precision above 2^53.
///
/// # Errors
///
/// [`ClientTsError`] names a contract type with no TypeScript spelling.
pub fn ts_type(type_name: &str) -> Result<&'static str, ClientTsError> {
    use wamn_schema_introspection::ir::ColumnType;

    if type_name == "string" {
        return Ok("string");
    }
    let column: ColumnType =
        serde_json::from_value(serde_json::Value::String(type_name.to_owned())).map_err(|_| {
            ClientTsError::new(
                ClientTsErrorKind::UnknownType,
                format!("contract type {type_name:?} has no TypeScript spelling"),
            )
        })?;
    Ok(match column {
        ColumnType::Boolean => "boolean",
        ColumnType::Int32 | ColumnType::Float64 => "number",
        ColumnType::Int64 => "Int64",
        ColumnType::Text => "string",
        ColumnType::Bytes => "number[]",
        ColumnType::Numeric => "Numeric",
        ColumnType::Timestamptz => "Timestamptz",
        ColumnType::Json => "JsonValue",
        ColumnType::Uuid => "Uuid",
    })
}

/// The TypeScript member name for one contract name.
///
/// An underscore becomes a capital only when a lowercase letter follows it, so
/// [`to_snake`] reverses every result.
#[must_use]
pub fn to_camel(name: &str) -> String {
    let mut member = String::with_capacity(name.len());
    let mut characters = name.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '_' && characters.peek().is_some_and(char::is_ascii_lowercase) {
            let letter = characters.next().expect("the peeked character is there");
            member.push(letter.to_ascii_uppercase());
        } else {
            member.push(character);
        }
    }
    member
}

/// The contract name for one TypeScript member name.
#[must_use]
pub fn to_snake(member: &str) -> String {
    let mut name = String::with_capacity(member.len() + 4);
    for character in member.chars() {
        if character.is_ascii_uppercase() {
            name.push('_');
            name.push(character.to_ascii_lowercase());
        } else {
            name.push(character);
        }
    }
    name
}

/// The hand-written package that every generated binding imports.
///
/// It holds the wire contract and the transport. `web/runtime` is its source,
/// and the application's own configuration resolves the specifier.
pub const RUNTIME_PACKAGE: &str = "@wamn/web-runtime";

/// The runtime version that generated bindings require.
///
/// The bindings and the runtime move together, so a package that generates
/// against this release states the range here rather than guessing.
pub const RUNTIME_VERSION_RANGE: &str = "^0.1.0";

/// Words that cannot name a TypeScript function or constant.
///
/// A contract name that takes one keeps it and gains a trailing underscore, in
/// the spirit of the Rust emitter's raw identifiers. `widget.delete` becomes
/// `delete_`, because `export async function delete` is a syntax error.
const RESERVED_WORDS: [&str; 39] = [
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "package",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
];

/// The type alias names that the shared module exports.
const ALIASES: [&str; 5] = ["Int64", "JsonValue", "Numeric", "Timestamptz", "Uuid"];

/// The operation kind that no browser calls.
const PRIVATE_KIND: &str = "event_handler";

/// Emit one TypeScript module per model, plus the shared module and an index.
///
/// Public operations only. An event handler emits nothing at all.
///
/// # Errors
///
/// [`ClientTsError`] names a contract identifier with no TypeScript spelling,
/// or two names that take the same TypeScript name.
pub fn emit_ts_client(ir: &ClientContractIr) -> Result<Vec<GeneratedFile>, ClientTsError> {
    let mut files = BTreeMap::new();
    let mut models: Vec<_> = ir.models.iter().collect();
    models.sort_by(|left, right| left.name.cmp(&right.name));
    let mut index = String::from("// @generated from the client-contract IR; do not edit.\n//\n");
    index.push_str("// The wire contract lives in `");
    index.push_str(RUNTIME_PACKAGE);
    index.push_str("`, which this package depends on.\n\n");
    let mut namespaces = BTreeSet::new();
    for model in &models {
        let namespace = ts_name(&model.name)?;
        if !namespaces.insert(namespace.clone()) {
            return Err(ClientTsError::new(
                ClientTsErrorKind::NameCollision,
                format!(
                    "model {:?} takes a TypeScript name another model took",
                    model.name
                ),
            ));
        }
        writeln!(
            index,
            "export * as {namespace} from \"./{}.js\";",
            model.name
        )
        .expect("writing to a String cannot fail");
        files.insert(
            format!("generated/client-ts/{}.ts", model.name),
            emit_model(&ir.package, model)?,
        );
    }
    files.insert("generated/client-ts/index.ts".to_owned(), index);
    Ok(files
        .into_iter()
        .map(|(path, source)| {
            GeneratedFile::new(
                path.into_boxed_str(),
                source.into_bytes().into_boxed_slice(),
            )
        })
        .collect())
}

/// The path of the package manifest inside a generated package.
pub const PACKAGE_JSON_PATH: &str = "generated/client-ts/package.json";

/// Emit the package manifest for the generated bindings.
///
/// The name is authored in the package manifest and never inferred. The
/// version is the release's own. The one dependency is the hand-written
/// runtime that the modules import, and the application's own configuration
/// resolves it.
#[must_use]
pub fn emit_ts_package_json(client_package: &str, version: &str) -> GeneratedFile {
    let source = format!(
        "{{\n  \"name\": {name},\n  \"version\": {version},\n  \"type\": \"module\",\n  \"private\": true,\n  \"exports\": {{\n    \".\": \"./index.ts\"\n  }},\n  \"dependencies\": {{\n    {runtime}: {range}\n  }}\n}}\n",
        name = serde_json::Value::String(client_package.to_owned()),
        version = serde_json::Value::String(version.to_owned()),
        runtime = serde_json::Value::String(RUNTIME_PACKAGE.to_owned()),
        range = serde_json::Value::String(RUNTIME_VERSION_RANGE.to_owned()),
    );
    GeneratedFile::new(
        PACKAGE_JSON_PATH.into(),
        source.into_bytes().into_boxed_slice(),
    )
}

fn emit_model(package: &str, model: &ModelIr) -> Result<String, ClientTsError> {
    let mut operations: Vec<_> = model
        .operations
        .iter()
        .filter(|operation| operation.kind != PRIVATE_KIND)
        .collect();
    operations.sort_by(|left, right| left.name.cmp(&right.name));

    let mut body = String::new();
    let mut used = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut served = false;
    check_reversible(
        &model.name,
        &format!("package {package} model {:?}", model.name),
    )?;
    for operation in &operations {
        check_reversible(&operation.name, &operation.operation)?;
        check_field_names(&operation.input_fields, &operation.operation)?;
        check_field_names(
            operation
                .route
                .as_ref()
                .map_or(operation.result_fields.as_slice(), |route| {
                    route.response.fields.as_slice()
                }),
            &operation.operation,
        )?;
        let function = ts_name(&operation.name)?;
        if !names.insert(function.clone()) {
            return Err(ClientTsError::new(
                ClientTsErrorKind::NameCollision,
                format!(
                    "operation {:?} takes the TypeScript name {function:?}, which another operation took",
                    operation.name
                ),
            ));
        }
        served |= operation.route.is_some();
        emit_operation(&mut body, model, operation, &function, &mut used)?;
    }

    let mut source = String::from("// @generated from the client-contract IR; do not edit.\n//\n");
    writeln!(
        source,
        "// `{}` operations of package `{package}`.",
        model.name
    )
    .expect("writing to a String cannot fail");
    if !operations.is_empty() {
        let mut types: BTreeSet<&str> = used.iter().copied().collect();
        // Every operation carries its field maps, served or not.
        types.insert("FieldMap");
        if served {
            types.extend(["OperationRoute", "Outcome", "Transport"]);
        }
        if !types.is_empty() {
            writeln!(
                source,
                "\nimport type {{ {} }} from \"{RUNTIME_PACKAGE}\";",
                types.into_iter().collect::<Vec<_>>().join(", ")
            )
            .expect("writing to a String cannot fail");
        }
        if served {
            writeln!(
                source,
                "import {{ reviveOutcome, toWire }} from \"{RUNTIME_PACKAGE}\";"
            )
            .expect("writing to a String cannot fail");
        }
    }
    source.push_str(&body);
    Ok(source)
}

fn emit_operation(
    source: &mut String,
    model: &ModelIr,
    operation: &OperationIr,
    function: &str,
    used: &mut BTreeSet<&'static str>,
) -> Result<(), ClientTsError> {
    let type_stem = type_stem(&model.name, &operation.name);
    let route_const = format!(
        "{}_{}_ROUTE",
        model.name.to_uppercase(),
        operation.name.to_uppercase()
    );
    let result_fields = operation
        .route
        .as_ref()
        .map_or(operation.result_fields.as_slice(), |route| {
            route.response.fields.as_slice()
        });

    let request_fields = format!("{}_REQUEST_FIELDS", route_const.trim_end_matches("_ROUTE"));
    let result_fields_const = format!("{}_RESULT_FIELDS", route_const.trim_end_matches("_ROUTE"));

    let sort = operation
        .paging
        .as_ref()
        .and_then(|paging| paging.sort.as_ref());
    match operation.description.as_deref() {
        // An authored description belongs to the operation, so it reaches the
        // type an author reads first. No screen shows it.
        Some(description) => writeln!(
            source,
            "\n/**\n * Input for `{}`.\n *\n * {description}\n */",
            operation.operation
        ),
        None => writeln!(source, "\n/** Input for `{}`. */", operation.operation),
    }
    .expect("write");
    write_interface(
        source,
        &format!("{type_stem}Request"),
        &operation.input_fields,
        used,
        sort,
        Carrier::Request,
    )?;
    writeln!(
        source,
        "\n/** What `{}` calls its input members. */",
        operation.operation
    )
    .expect("write");
    write_field_map(source, &request_fields, &operation.input_fields)?;
    let result_class = operation.route.as_ref().map_or_else(
        || Some(operation.result_class.as_str()),
        |route| route.response.result_class.as_deref(),
    );
    write_result(
        source,
        &type_stem,
        &operation.operation,
        result_class,
        result_fields,
        used,
    )?;
    writeln!(
        source,
        "\n/** What `{}` calls its result members. */",
        operation.operation
    )
    .expect("write");
    write_result_field_map(source, &result_fields_const, result_class, result_fields)?;

    let Some(route) = &operation.route else {
        // Stated, not silently omitted, for the reason `client_rust.rs` gives.
        writeln!(
            source,
            "\n// `{}` is not published over HTTP by this release, so it has no route\n// and no invoke function. It remains listed for its types.",
            operation.operation
        )
        .expect("write");
        return Ok(());
    };

    let replay = match route.replay {
        Some(ReplayIr::Claim) => "\"claim\"",
        Some(ReplayIr::State) => "\"state\"",
        None => "null",
    };
    writeln!(
        source,
        "\n/**\n * Where the release publishes `{}`.\n *\n * Method and template only. The host and base URL are the application's\n * deployment configuration, not this release's facts.\n */",
        operation.operation
    )
    .expect("write");
    writeln!(source, "export const {route_const}: OperationRoute = {{").expect("write");
    writeln!(source, "  operation: {:?},", operation.operation).expect("write");
    writeln!(source, "  method: {:?},", route.method).expect("write");
    writeln!(source, "  template: {:?},", route.template).expect("write");
    writeln!(source, "  freshOnly: {},", operation.fresh_only).expect("write");
    source.push_str("  contract: {\n");
    writeln!(
        source,
        "    resultClass: {},",
        route
            .response
            .result_class
            .as_deref()
            .map_or_else(|| "null".to_owned(), |class| format!("{class:?}"))
    )
    .expect("write");
    writeln!(
        source,
        "    partialSchema: {},",
        route.response.partial_schema.as_ref().map_or_else(
            || "null".to_owned(),
            |schema| format!("{:?}", schema.to_string())
        )
    )
    .expect("write");
    source.push_str("    errors: [\n");
    for error in &route.response.errors {
        writeln!(
            source,
            "      {{ literal: {:?}, required: [{}], sources: [{}] }},",
            error.literal,
            literals(&error.detail_required),
            literals(&error.sources)
        )
        .expect("write");
    }
    source.push_str("    ],\n");
    writeln!(source, "    replay: {replay},").expect("write");
    writeln!(source, "    direct: {},", route.direct).expect("write");
    writeln!(source, "    kind: {:?},", operation.kind).expect("write");
    writeln!(
        source,
        "    transaction: {},",
        operation
            .transaction
            .as_deref()
            .map_or_else(|| "null".to_owned(), |value| format!("{value:?}"))
    )
    .expect("write");
    source.push_str("  },\n};\n");

    writeln!(
        source,
        "\n/** Invoke `{}` through a transport the application supplies. */",
        operation.operation
    )
    .expect("write");
    writeln!(
        source,
        "export async function {function}(\n  transport: Transport,\n  items: readonly {type_stem}Request[],\n): Promise<Outcome<{type_stem}Result>> {{"
    )
    .expect("write");
    writeln!(
        source,
        "  return reviveOutcome<{type_stem}Result>(\n    await transport.invoke({{\n      ...{route_const},\n      items: items.map((item) => toWire(item, {request_fields})),\n    }}),\n    {result_fields_const},\n  );\n}}"
    )
    .expect("write");
    Ok(())
}

/// Emit the field map of one operation's result.
///
/// A bounded list and a page carry their rows inside an envelope, so the map
/// describes the envelope: the row collection under its own key, and the page
/// cursor beside it. The row members sit one level down, exactly where the
/// wire carries them.
fn write_result_field_map(
    source: &mut String,
    name: &str,
    result_class: Option<&str>,
    fields: &[FieldIr],
) -> Result<(), ClientTsError> {
    let key = match result_class {
        Some("bounded_list") => "rows",
        Some("page") => "item",
        _ => return write_field_map(source, name, fields),
    };
    writeln!(source, "export const {name}: FieldMap = {{").expect("write");
    writeln!(source, "  {key:?}: {{").expect("write");
    writeln!(source, "    member: {key:?},").expect("write");
    if fields.is_empty() {
        writeln!(source, "    fields: {{}},").expect("write");
    } else {
        writeln!(source, "    fields: {{").expect("write");
        write_map_members(source, fields, 3)?;
        writeln!(source, "    }},").expect("write");
    }
    writeln!(source, "  }},").expect("write");
    if key == "item" {
        writeln!(source, "  \"next_cursor\": \"nextCursor\",").expect("write");
    }
    writeln!(source, "}};").expect("write");
    Ok(())
}

/// Emit one declared shape's field map.
///
/// The key is the wire key, quoted. A string value is the member name. An
/// object value is a declared object or array, whose members sit under
/// `fields`. A `json` member has no members to declare, so it is a plain
/// string entry and the run time copies its value untouched.
fn write_field_map(
    source: &mut String,
    name: &str,
    fields: &[FieldIr],
) -> Result<(), ClientTsError> {
    if fields.is_empty() {
        writeln!(source, "export const {name}: FieldMap = {{}};").expect("write");
        return Ok(());
    }
    writeln!(source, "export const {name}: FieldMap = {{").expect("write");
    write_map_members(source, fields, 1)?;
    writeln!(source, "}};").expect("write");
    Ok(())
}

fn write_map_members(
    source: &mut String,
    fields: &[FieldIr],
    depth: usize,
) -> Result<(), ClientTsError> {
    let indent = "  ".repeat(depth);
    for field in fields {
        let leaf = field
            .path
            .rsplit('.')
            .next()
            .unwrap_or(&field.path)
            .trim_end_matches("[]");
        if leaf.is_empty() {
            continue;
        }
        let member = ts_member(leaf, &field.path)?;
        // The one array shape that carries no members: repeated scalars.
        let scalar_items = field.children.len() == 1 && field.children[0].path == field.path;
        if matches!(field.type_name.as_str(), "object" | "array")
            && !field.children.is_empty()
            && !scalar_items
        {
            writeln!(source, "{indent}{leaf:?}: {{").expect("write");
            writeln!(source, "{indent}  member: {member:?},").expect("write");
            writeln!(source, "{indent}  fields: {{").expect("write");
            write_map_members(source, &field.children, depth + 2)?;
            writeln!(source, "{indent}  }},").expect("write");
            writeln!(source, "{indent}}},").expect("write");
        } else {
            writeln!(source, "{indent}{leaf:?}: {member:?},").expect("write");
        }
    }
    Ok(())
}

/// One TypeScript list of string literals.
fn literals(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("{value:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Emit the result type of one operation, and its row type when the release
/// serves a collection.
///
/// A bounded list and a page do not return one record: they return an envelope
/// that carries the rows. `validate_value` in
/// `crates/client/tui/src/submission.rs` states it. A bounded list carries its
/// array under `rows`. A page carries its array under `item`, beside a
/// `next_cursor` that is present and either null or a string. The envelope is
/// what the transport returns, so the envelope is the result type, and the row
/// keeps its own name.
fn write_result(
    source: &mut String,
    type_stem: &str,
    operation: &str,
    result_class: Option<&str>,
    fields: &[FieldIr],
    used: &mut BTreeSet<&'static str>,
) -> Result<(), ClientTsError> {
    let paged = match result_class {
        Some("bounded_list") => false,
        Some("page") => true,
        _ => {
            writeln!(source, "\n/** Result of `{operation}`. */").expect("write");
            return write_interface(
                source,
                &format!("{type_stem}Result"),
                fields,
                used,
                None,
                Carrier::Result,
            );
        }
    };
    writeln!(source, "\n/** One row of `{operation}`. */").expect("write");
    write_interface(
        source,
        &format!("{type_stem}Row"),
        fields,
        used,
        None,
        Carrier::Result,
    )?;
    writeln!(source, "\n/** Result of `{operation}`. */").expect("write");
    writeln!(source, "export interface {type_stem}Result {{").expect("write");
    if paged {
        writeln!(source, "  /** The rows this page carries. */").expect("write");
        writeln!(source, "  readonly item: readonly {type_stem}Row[];").expect("write");
        writeln!(
            source,
            "  /** The next page's cursor, or null at the last page. */"
        )
        .expect("write");
        writeln!(source, "  readonly nextCursor: string | null;").expect("write");
    } else {
        writeln!(source, "  /** Every row the release served. */").expect("write");
        writeln!(source, "  readonly rows: readonly {type_stem}Row[];").expect("write");
    }
    writeln!(source, "}}").expect("write");
    Ok(())
}

/// The closed value domain of one field, when the release declares one.
///
/// A field states its own domain. The sortable fields and the permitted
/// directions sit beside the input, in the paging contract, so the sort is
/// passed in and matched by its exact path.
fn domain<'a>(field: &'a FieldIr, sort: Option<&'a SortIr>) -> Option<&'a [String]> {
    if !field.values.is_empty() {
        return Some(field.values.as_slice());
    }
    match (sort, field.path.as_str()) {
        (Some(sort), SORT_FIELD_INPUT) => Some(sort.fields.as_slice()),
        (Some(sort), SORT_DIRECTION_INPUT) => Some(sort.directions.as_slice()),
        _ => None,
    }
}

/// One scalar's spelling: its declared domain, or the type map.
fn scalar_spelling(
    type_name: &str,
    values: Option<&[String]>,
    used: &mut BTreeSet<&'static str>,
) -> String {
    match values {
        Some(values) if !values.is_empty() => values
            .iter()
            .map(|value| format!("{value:?}"))
            .collect::<Vec<_>>()
            .join(" | "),
        _ => record(ts_type(type_name).unwrap_or("JsonValue"), used).to_owned(),
    }
}

/// Whether one interface describes what a caller sends or what a release
/// returned.
///
/// A result is a fact that came back, so its members are read only. A request
/// is a value the caller builds, and a form library writes into it one member
/// at a time, so its members stay writable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Carrier {
    Request,
    Result,
}

impl Carrier {
    /// The prefix of one member declaration.
    const fn prefix(self) -> &'static str {
        match self {
            Self::Request => "",
            Self::Result => "readonly ",
        }
    }
}

fn write_interface(
    source: &mut String,
    name: &str,
    fields: &[FieldIr],
    used: &mut BTreeSet<&'static str>,
    sort: Option<&SortIr>,
    carrier: Carrier,
) -> Result<(), ClientTsError> {
    writeln!(source, "export interface {name} {{").expect("write");
    let mut nested = Vec::new();
    for field in fields {
        let leaf = field
            .path
            .rsplit('.')
            .next()
            .unwrap_or(&field.path)
            .trim_end_matches("[]");
        if leaf.is_empty() {
            continue;
        }
        let member = ts_member(leaf, &field.path)?;
        let child_name = format!("{name}{}", pascal_case(leaf));
        let mut spelling = match field.type_name.as_str() {
            "object" if !field.children.is_empty() => {
                nested.push((child_name.clone(), field.children.as_slice()));
                child_name
            }
            "array" if !field.children.is_empty() => {
                let item = if field.children.len() == 1 && field.children[0].path == field.path {
                    let child = &field.children[0];
                    let item = scalar_spelling(&child.type_name, domain(child, sort), used);
                    // A union needs its own parentheses inside an array.
                    if item.contains(" | ") {
                        format!("({item})")
                    } else {
                        item
                    }
                } else {
                    nested.push((child_name.clone(), field.children.as_slice()));
                    child_name
                };
                match carrier {
                    Carrier::Request => format!("{item}[]"),
                    Carrier::Result => format!("readonly {item}[]"),
                }
            }
            other => scalar_spelling(other, domain(field, sort), used),
        };
        if field.nullable {
            spelling = format!("{spelling} | null");
        }
        let wire = format!(
            "`{}`{}",
            field.type_name,
            if field.required { "" } else { ", omittable" }
        );
        match field.description.as_deref() {
            // The authored sentence comes first, because a reader wants the
            // meaning before the wire spelling.
            Some(description) => writeln!(
                source,
                "  /**\n   * {description}\n   *\n   * {wire}\n   */"
            ),
            None => writeln!(source, "  /** {wire} */"),
        }
        .expect("write");
        writeln!(
            source,
            "  {}{member}{}: {spelling};",
            carrier.prefix(),
            if field.required { "" } else { "?" }
        )
        .expect("write");
    }
    writeln!(source, "}}").expect("write");
    for (child_name, children) in nested {
        source.push('\n');
        write_interface(source, &child_name, children, used, sort, carrier)?;
    }
    Ok(())
}

/// Record a referenced alias so the module imports exactly what it uses.
fn record<'a>(spelling: &'a str, used: &mut BTreeSet<&'static str>) -> &'a str {
    if let Some(alias) = ALIASES.iter().find(|alias| **alias == spelling) {
        used.insert(alias);
    }
    spelling
}

/// The type stem that one operation's interfaces share.
///
/// The component emitter names the same types, so the rule lives here beside
/// the emitter that writes them.
#[must_use]
pub fn type_stem(model: &str, operation: &str) -> String {
    format!("{}{}", pascal_case(model), pascal_case(operation))
}

/// The type stem of one operation, from its canonical identity.
///
/// A row link states the operation it opens by identity, and a component names
/// that operation's types. An identity spells a name with hyphens where the
/// contract spells it with underscores, so the hyphens turn back first.
#[must_use]
pub fn operation_stem(identity: &str) -> String {
    let after_package = identity.split_once(':').map_or(identity, |(_, rest)| rest);
    let without_version = after_package
        .split_once('@')
        .map_or(after_package, |(name, _)| name);
    let contract_name = |name: &str| name.replace('-', "_");
    match without_version.split_once('/') {
        Some((model, operation)) => type_stem(&contract_name(model), &contract_name(operation)),
        None => pascal_case(&contract_name(without_version)),
    }
}

/// The exported function name of one operation.
///
/// # Errors
///
/// [`ClientTsError`] names a contract name with no TypeScript spelling.
pub fn function_name(operation: &str) -> Result<String, ClientTsError> {
    ts_name(operation)
}

/// The PascalCase type stem for one contract name.
fn pascal_case(name: &str) -> String {
    let member = to_camel(name);
    let mut characters = member.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_ascii_uppercase().to_string() + characters.as_str()
    })
}

/// The exported TypeScript name for one contract name.
fn ts_name(name: &str) -> Result<String, ClientTsError> {
    let member = identifier(name, name)?;
    Ok(if RESERVED_WORDS.contains(&member.as_str()) {
        format!("{member}_")
    } else {
        member
    })
}

/// The interface member name for one contract leaf.
///
/// A member may spell a reserved word, so it needs no escape.
fn ts_member(leaf: &str, path: &str) -> Result<String, ClientTsError> {
    identifier(leaf, path)
}

/// Refuse a contract name that does not reverse through the member rule.
///
/// The field map holds both spellings, so conversion works either way. The
/// refusal keeps the two spellings one to one, so a reader who maps a member
/// name back reaches the contract name that emitted it. `line_1` reverses,
/// because an underscore becomes a capital only before a lowercase letter. A
/// capital inside a contract name does not reverse.
fn check_reversible(name: &str, subject: &str) -> Result<(), ClientTsError> {
    let member = to_camel(name);
    let back = to_snake(&member);
    if back == name {
        return Ok(());
    }
    Err(ClientTsError::new(
        ClientTsErrorKind::IrreversibleName,
        format!("{subject} takes the member {member:?}, which maps back to {back:?}"),
    ))
}

/// Refuse every declared name of one shape that does not reverse.
fn check_field_names(fields: &[FieldIr], subject: &str) -> Result<(), ClientTsError> {
    for field in fields {
        let leaf = field
            .path
            .rsplit('.')
            .next()
            .unwrap_or(&field.path)
            .trim_end_matches("[]");
        if !leaf.is_empty() {
            check_reversible(leaf, &format!("{subject} field {:?}", field.path))?;
        }
        check_field_names(&field.children, subject)?;
    }
    Ok(())
}

fn identifier(name: &str, subject: &str) -> Result<String, ClientTsError> {
    let member = to_camel(name);
    let valid = member
        .bytes()
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
        && member
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid {
        Ok(member)
    } else {
        Err(ClientTsError::new(
            ClientTsErrorKind::UnnameableIdentifier,
            format!("{subject:?} is not a TypeScript identifier"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The type map is a contract, and the compile gate cannot check it: a
    /// `uuid` emitted as `string` still type-checks, because the alias IS a
    /// string. The map is pinned here directly, as `client_rust.rs` pins its
    /// own.
    #[test]
    fn every_contract_type_has_its_exact_typescript_spelling() {
        for (contract, typescript) in [
            ("boolean", "boolean"),
            ("int32", "number"),
            ("int64", "Int64"),
            ("float64", "number"),
            ("text", "string"),
            ("bytes", "number[]"),
            ("numeric", "Numeric"),
            ("timestamptz", "Timestamptz"),
            ("json", "JsonValue"),
            ("uuid", "Uuid"),
            // The one literal outside the frozen column vocabulary: generated
            // CRUD input contracts spell `request_id` this way.
            ("string", "string"),
        ] {
            assert_eq!(
                ts_type(contract).unwrap_or_else(|error| panic!("{contract}: {error}")),
                typescript,
                "{contract}"
            );
        }
    }

    /// An unknown type REFUSES, for the reason `client_rust.rs` states: an
    /// approximation compiles and describes the wrong shape.
    #[test]
    fn an_unknown_contract_type_refuses_by_name() {
        let refusal = ts_type("geography").expect_err("an unmapped type refuses");
        assert_eq!(refusal.kind(), ClientTsErrorKind::UnknownType);
        assert!(refusal.to_string().contains("geography"), "{refusal}");
        assert!(
            refusal.to_string().starts_with("unknown_type: "),
            "{refusal}"
        );
    }

    /// An identity spells `purchase_order` as `purchase-order`, and the types
    /// keep the contract's own spelling.
    #[test]
    fn an_operation_stem_reads_the_contract_name_out_of_an_identity() {
        for (identity, stem) in [
            ("platform-fixture:widget/get@1.0.0", "WidgetGet"),
            (
                "wamn-receiving:purchase-order/get@1.0.0",
                "PurchaseOrderGet",
            ),
            (
                "wamn-receiving:receipt/record-receipt@1.0.0",
                "ReceiptRecordReceipt",
            ),
        ] {
            assert_eq!(operation_stem(identity), stem, "{identity}");
        }
    }

    #[test]
    fn the_member_name_rule_reverses_every_contract_name() {
        for (contract, member) in [
            ("id", "id"),
            ("request_id", "requestId"),
            ("expected_row_version", "expectedRowVersion"),
            ("value", "value"),
            // A digit keeps its underscore, so the rule stays reversible.
            ("line_1", "line_1"),
            ("po_line_2_note", "poLine_2Note"),
        ] {
            assert_eq!(to_camel(contract), member, "{contract}");
            assert_eq!(to_snake(member), contract, "{member}");
        }
    }

    /// The emitted `convertKeys` walks arrays and objects and renames every
    /// key. This applies the same pair over a nested and repeated shape, so a
    /// name rule that reverses one name also reverses a whole document.
    #[test]
    fn a_nested_and_repeated_wire_object_survives_the_round_trip() {
        fn convert(value: &serde_json::Value, key: fn(&str) -> String) -> serde_json::Value {
            match value {
                serde_json::Value::Array(items) => {
                    serde_json::Value::Array(items.iter().map(|item| convert(item, key)).collect())
                }
                serde_json::Value::Object(members) => serde_json::Value::Object(
                    members
                        .iter()
                        .map(|(name, member)| (key(name), convert(member, key)))
                        .collect(),
                ),
                other => other.clone(),
            }
        }

        let wire = serde_json::json!({
            "request_id": "9a1c",
            "expected_row_version": "7",
            "value": {
                "line_1": null,
                "received_lines": [
                    {"line_number": 1, "unit_price": "1.50", "note": "ok"},
                    {"line_number": 2, "unit_price": "0.00", "note": null}
                ]
            }
        });
        let members = convert(&wire, to_camel);
        assert!(members.pointer("/requestId").is_some());
        assert!(
            members
                .pointer("/value/receivedLines/0/lineNumber")
                .is_some()
        );
        assert!(members.pointer("/value/line_1").is_some());
        assert_eq!(convert(&members, to_snake), wire);
    }
}
