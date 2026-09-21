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
//! camelCase. [`to_camel`] decides the member names at emission, and the
//! emitted `fromWire` applies the same rule at run time. The rule is
//! reversible: an underscore becomes a capital only when a lowercase letter
//! follows it, so `line_1` keeps its underscore.

use std::fmt::Write as _;

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

/// The path of the shared module inside a generated package.
pub const WIRE_MODULE_PATH: &str = "generated/client-ts/wire.ts";

/// Emit the shared module that every generated binding imports.
///
/// It declares the type aliases, the transport interface, the four outcomes of
/// one intent, and the one name mapping pair. It declares no operation and no
/// deployment fact, so its bytes do not depend on the contract.
#[must_use]
pub fn wire_module() -> String {
    let mut source = String::new();
    writeln!(
        source,
        "// @generated from the client-contract IR; do not edit."
    )
    .expect("writing to a String cannot fail");
    source.push_str(WIRE_MODULE_BODY);
    source
}

/// The body of the shared module.
///
/// One place holds the TypeScript spelling of the name mapping. [`to_camel`]
/// and [`to_snake`] hold the same rule for emission.
const WIRE_MODULE_BODY: &str = r#"
/** A UUID in hyphenated form. */
export type Uuid = string;

/** An RFC 3339 timestamp in UTC, to microseconds. */
export type Timestamptz = string;

/** A 64-bit integer as a decimal string. A number loses precision above 2^53. */
export type Int64 = string;

/** An exact decimal as a canonical string. */
export type Numeric = string;

/** Any JSON value. */
export type JsonValue =
  | null
  | boolean
  | number
  | string
  | readonly JsonValue[]
  | { readonly [key: string]: JsonValue };

/** What one operation's response contract states. A transport classifies with it. */
export interface ResponseContract {
  /** `one`, `bounded_list`, `page`, `none`, or null when the release states none. */
  readonly resultClass: string | null;
  /** The partial completion schema, as published JSON text. */
  readonly partialSchema: string | null;
  /** Every refusal literal the operation declares. */
  readonly errors: readonly string[];
  /** The replay guarantee the release serves. */
  readonly replay: "claim" | "state" | null;
}

/** One request. It carries no host, no base URL and no credential. */
export interface WireRequest {
  /** The exact canonical operation identity. */
  readonly operation: string;
  /** The method the release publishes. */
  readonly method: string;
  /** The path template the release publishes. */
  readonly template: string;
  /** Whether the operation admits only a fresh credential. */
  readonly freshOnly: boolean;
  /** What the response must satisfy. */
  readonly contract: ResponseContract;
  /** The submitted items, in wire spelling. */
  readonly items: readonly JsonValue[];
}

/**
 * The outcome of one submitted intent.
 *
 * The four members carry the whole meaning: the intent completed, it completed
 * in part, the operation refused it, or its completion is unknown. A caller
 * branches on `status` and never catches an exception.
 */
export type Outcome<T> =
  | { readonly status: "completed"; readonly value: T }
  | {
      readonly status: "partiallyCompleted";
      readonly committedResult: T;
      readonly failedOutcome: JsonValue;
    }
  | { readonly status: "refused"; readonly code: string; readonly detail: JsonValue }
  | {
      readonly status: "uncertain";
      readonly reason: string;
      readonly retryRefusal: JsonValue | null;
    };

/**
 * The transport an application supplies.
 *
 * It owns the URL, the credential, the request envelope and the classification
 * of the response into one `Outcome`. The bindings only state what to send.
 */
export interface Transport {
  invoke(request: WireRequest): Promise<Outcome<JsonValue>>;
}

function convertKeys(value: JsonValue, key: (name: string) => string): JsonValue {
  if (Array.isArray(value)) {
    return value.map((item) => convertKeys(item, key));
  }
  if (value !== null && typeof value === "object") {
    const converted: { [name: string]: JsonValue } = {};
    for (const [name, member] of Object.entries(value)) {
      converted[key(name)] = convertKeys(member, key);
    }
    return converted;
  }
  return value;
}

function toCamel(name: string): string {
  return name.replace(/_([a-z])/g, (_match, letter: string) => letter.toUpperCase());
}

function toSnake(member: string): string {
  return member.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`);
}

/** Convert wire keys to TypeScript members. */
export function fromWire(value: JsonValue): JsonValue {
  return convertKeys(value, toCamel);
}

/** Convert TypeScript members to wire keys. */
export function toWire(value: JsonValue): JsonValue {
  return convertKeys(value, toSnake);
}
"#;

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

    #[test]
    fn the_shared_module_is_byte_stable_and_states_no_deployment_fact() {
        let first = wire_module();
        assert_eq!(first, wire_module());
        assert!(first.starts_with("// @generated from the client-contract IR; do not edit.\n"));
        for declaration in [
            "export type Uuid = string;",
            "export type Int64 = string;",
            "export type Numeric = string;",
            "export type Timestamptz = string;",
            "export type JsonValue =",
            "export interface ResponseContract {",
            "export interface WireRequest {",
            "export type Outcome<T> =",
            "export interface Transport {",
            "export function fromWire(value: JsonValue): JsonValue {",
            "export function toWire(value: JsonValue): JsonValue {",
        ] {
            assert!(first.contains(declaration), "{declaration}");
        }
        for status in [
            "\"completed\"",
            "\"partiallyCompleted\"",
            "\"refused\"",
            "\"uncertain\"",
        ] {
            assert_eq!(first.matches(status).count(), 1, "{status}");
        }
        for absent in [
            "http",
            "localhost",
            "Authorization",
            "Bearer",
            "baseUrl",
            "fetch(",
            "import ",
        ] {
            assert!(
                !first.contains(absent),
                "deployment or framework fact {absent}"
            );
        }
    }
}
