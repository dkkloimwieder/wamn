//! The canonical client-contract IR.
//!
//! ONE generator input: the effective-release contract projection under
//! `packages/<package>/generated/contracts/`. `wamn.json` is a contributor to
//! that projection, not the boundary — the IR describes what a release
//! actually ships, so a client generated from it cannot drift from the
//! deployed contract by reading the authoring manifest instead.
//!
//! # Why an IR at all
//!
//! Language emitters consume this, never the contract files directly. Rust is
//! the only emitter built now; TypeScript is deferred. The IR is what keeps
//! that honest: without it, the first emitter's implementation details become
//! the second emitter's contract by accident.
//!
//! # Byte stability
//!
//! Every collection is ordered deterministically and the whole IR serializes
//! through the platform's shared canonicalization, so regenerating from an
//! unchanged release yields identical bytes. That is the property the exit
//! gate asserts, and it is why nothing here iterates a `HashMap`.
//!
//! Canonical means order-independent **only for sets**. Some contract members
//! are ordered on purpose — `cursor.member_order` IS an ordering — and an IR
//! that reported identical bytes after reversing one of those would be wrong,
//! not canonical. The reorder-and-compare gate therefore has to know which
//! lists are sets; a version of it that reversed every array failed against a
//! correct IR before that distinction was drawn.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// IR shape version. A consumer that does not recognise it must refuse rather
/// than guess at a field's meaning.
pub const CLIENT_IR_FORMAT_VERSION: u32 = 1;

/// Why a contract projection could not be read as an IR.
#[derive(Debug)]
pub struct ClientIrError {
    kind: ClientIrErrorKind,
    detail: String,
}

impl ClientIrError {
    /// Stable refusal class.
    #[must_use]
    pub const fn kind(&self) -> ClientIrErrorKind {
        self.kind
    }

    fn new(kind: ClientIrErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for ClientIrError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.kind.code(), self.detail)
    }
}

impl std::error::Error for ClientIrError {}

/// Stable classification for an IR refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientIrErrorKind {
    /// The contract directory could not be read.
    UnreadableProjection,
    /// A contract file was not the JSON the projection declares.
    MalformedContract,
    /// A required contract member was absent.
    MissingMember,
}

impl ClientIrErrorKind {
    /// Stable wire code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnreadableProjection => "unreadable_projection",
            Self::MalformedContract => "malformed_contract",
            Self::MissingMember => "missing_member",
        }
    }
}

/// One package's client contract, normalized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ClientContractIr {
    /// IR shape version.
    pub format_version: u32,
    /// The package this IR was projected from.
    pub package: String,
    /// The shared cursor contract, when the package pages anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Value>,
    /// Models, ordered by name.
    pub models: Vec<ModelIr>,
}

/// One model and everything a client needs to work with it.
///
/// A model is a contract MODULE — the projection groups operations under one,
/// and that grouping is the model boundary a client sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ModelIr {
    /// Module name, e.g. `purchase_order`.
    pub name: String,
    /// Field descriptors, unioned across this model's result contracts and
    /// ordered by path. This is the descriptor set primitive controls consume
    /// directly; nothing else hand-authors descriptors.
    pub fields: Vec<FieldIr>,
    /// Operations, ordered by name.
    pub operations: Vec<OperationIr>,
}

/// One field descriptor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct FieldIr {
    /// Dotted path within its carrier, e.g. `value.purchase_order_id`.
    pub path: String,
    /// Contract type name, e.g. `uuid`, `text`, `timestamptz`, `int64`.
    pub type_name: String,
    /// Whether the contract admits null.
    pub nullable: bool,
    /// Closed value domain, when the contract declares one. Empty means open.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

/// One operation, as a client must call it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct OperationIr {
    /// Local name within the model, e.g. `query`.
    pub name: String,
    /// Canonical operation identity.
    pub operation: String,
    /// The grant a caller needs.
    pub grant: String,
    /// Permission token this operation is authorized by.
    pub permission_token: String,
    /// Result class, e.g. `one`, `page`.
    pub result_class: String,
    /// Input field descriptors, ordered by path.
    pub input_fields: Vec<FieldIr>,
    /// Result field descriptors, ordered by path.
    pub result_fields: Vec<FieldIr>,
    /// Fields the server owns; supplying one is refused.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub server_owned_fields: Vec<String>,
    /// Array-envelope bounds, when the operation takes one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub envelope: Option<Value>,
    /// Filter, sort, limit and pagination contract, when the operation pages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paging: Option<PagingIr>,
    /// Typed error cases, ordered by literal.
    pub errors: Vec<ErrorCaseIr>,
}

/// Filters, sort and pagination for one operation.
///
/// Typed rather than passed through as contract JSON. An opaque blob is not an
/// IR: it re-exports the input's incidental ordering, so two releases that say
/// the same thing produce different bytes, and an emitter has to re-parse what
/// this layer already read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PagingIr {
    /// Declared filters, ordered by field.
    pub filters: Vec<FilterIr>,
    /// Sortable fields and permitted directions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<SortIr>,
    /// Limit bounds and default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<LimitIr>,
    /// Pagination kind, cursor encoding, default sort and tie breaker. Left as
    /// contract JSON deliberately: its members carry semantic order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagination: Option<Value>,
}

/// One declared filter.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct FilterIr {
    /// The field this filter narrows.
    pub field: String,
    /// How the value binds, e.g. `json_array`.
    pub binding: String,
}

/// The sort contract: which fields, which directions, how many at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SortIr {
    /// Sortable fields — a SET, ordered here so the IR is canonical.
    pub fields: Vec<String>,
    /// Permitted directions, likewise a set.
    pub directions: Vec<String>,
    /// How many sort fields one request may name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_fields: Option<u64>,
}

/// Page-size bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LimitIr {
    /// Applied when the caller names none.
    pub default: u64,
    /// Smallest accepted page size.
    pub minimum: u64,
    /// Largest accepted page size.
    pub maximum: u64,
}

/// One typed error case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ErrorCaseIr {
    /// The wire literal a client branches on.
    pub literal: String,
    /// Detail members always present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail_required: Vec<String>,
    /// Detail members that may be present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detail_optional: Vec<String>,
}

impl ClientContractIr {
    /// Canonical bytes — the comparable form the byte-stability gate compares.
    ///
    /// Goes through the platform's shared canonicalization rather than
    /// `serde_json::to_vec`, so an IR digest is comparable with every other
    /// digest in the platform.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        wamn_execution_contract::canonical_json_bytes(
            &serde_json::to_value(self).expect("the IR always serializes"),
        )
    }

    /// Project one package's emitted contract directory into an IR.
    ///
    /// # Errors
    ///
    /// [`ClientIrError`] naming which contract could not be read.
    pub fn from_contract_directory(package: &str, contracts: &Path) -> Result<Self, ClientIrError> {
        let mut modules: BTreeMap<String, BTreeMap<String, OperationParts>> = BTreeMap::new();
        let mut cursor = None;

        let entries = std::fs::read_dir(contracts).map_err(|error| {
            ClientIrError::new(
                ClientIrErrorKind::UnreadableProjection,
                format!("read {}: {error}", contracts.display()),
            )
        })?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let module = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default()
                    .to_owned();
                collect_module(&path, modules.entry(module).or_default())?;
            } else if path
                .file_name()
                .is_some_and(|name| name == "cursor-v1.json")
            {
                cursor = Some(read_json(&path)?);
            }
        }

        let models = modules
            .into_iter()
            .map(|(name, operations)| build_model(&name, operations))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            format_version: CLIENT_IR_FORMAT_VERSION,
            package: package.to_owned(),
            cursor,
            models,
        })
    }
}

/// The four contract files for one operation, before normalization.
#[derive(Default)]
struct OperationParts {
    operation: Option<Value>,
    input: Option<Value>,
    result: Option<Value>,
    errors: Option<Value>,
}

fn collect_module(
    directory: &Path,
    operations: &mut BTreeMap<String, OperationParts>,
) -> Result<(), ClientIrError> {
    let entries = std::fs::read_dir(directory).map_err(|error| {
        ClientIrError::new(
            ClientIrErrorKind::UnreadableProjection,
            format!("read {}: {error}", directory.display()),
        )
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some((operation, part)) = split_contract_name(file_name) else {
            continue;
        };
        let value = read_json(&path)?;
        let slot = operations.entry(operation.to_owned()).or_default();
        match part {
            "operation" => slot.operation = Some(value),
            "input" => slot.input = Some(value),
            "result" => slot.result = Some(value),
            "errors" => slot.errors = Some(value),
            _ => {}
        }
    }
    Ok(())
}

/// `get.input.json` -> (`get`, `input`).
fn split_contract_name(file_name: &str) -> Option<(&str, &str)> {
    let stem = file_name.strip_suffix(".json")?;
    let (operation, part) = stem.rsplit_once('.')?;
    Some((operation, part))
}

fn read_json(path: &Path) -> Result<Value, ClientIrError> {
    let source = std::fs::read_to_string(path).map_err(|error| {
        ClientIrError::new(
            ClientIrErrorKind::UnreadableProjection,
            format!("read {}: {error}", path.display()),
        )
    })?;
    serde_json::from_str(&source).map_err(|error| {
        ClientIrError::new(
            ClientIrErrorKind::MalformedContract,
            format!("{} is not contract JSON: {error}", path.display()),
        )
    })
}

fn build_model(
    name: &str,
    operations: BTreeMap<String, OperationParts>,
) -> Result<ModelIr, ClientIrError> {
    let mut built = Vec::with_capacity(operations.len());
    for (operation_name, parts) in operations {
        if let Some(operation) = build_operation(name, &operation_name, parts)? {
            built.push(operation);
        }
    }
    // The model's descriptor set is the union of what its operations return.
    // Unioned rather than taken from one operation, because `get` and `query`
    // may each project a subset and a control needs the whole field.
    let mut fields: Vec<FieldIr> = built
        .iter()
        .flat_map(|operation| operation.result_fields.clone())
        .collect();
    fields.sort();
    fields.dedup();
    Ok(ModelIr {
        name: name.to_owned(),
        fields,
        operations: built,
    })
}

/// One operation, or `None` when the contract describes something a client
/// cannot call.
///
/// A package's contract directory is not a client surface. It also carries
/// PRIVATE operations — `client_acme_receiving`'s `quality/create_inspection`
/// is an `event_handler` with `visibility: private`, a null `grant` and a null
/// `permission_token` — which the platform invokes internally and no caller
/// ever addresses. Projecting one would put an operation in a client that has
/// no grant to present, no route to reach, and no caller.
fn build_operation(
    module: &str,
    name: &str,
    parts: OperationParts,
) -> Result<Option<OperationIr>, ClientIrError> {
    let operation = parts.operation.ok_or_else(|| {
        ClientIrError::new(
            ClientIrErrorKind::MissingMember,
            format!("{module}/{name} has no operation contract"),
        )
    })?;
    // Excluded by DECLARATION, never by a missing member: a public operation
    // whose grant is absent is a malformed contract and must still refuse
    // below, not vanish from the client because a member failed to parse.
    if operation.get("visibility").and_then(Value::as_str) == Some("private") {
        return Ok(None);
    }
    let member = |key: &str| -> Result<String, ClientIrError> {
        operation
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                ClientIrError::new(
                    ClientIrErrorKind::MissingMember,
                    format!("{module}/{name} operation contract has no {key:?}"),
                )
            })
    };

    let input = parts
        .input
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let result = parts
        .result
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

    Ok(Some(OperationIr {
        name: name.to_owned(),
        operation: member("operation")?,
        grant: member("grant")?,
        permission_token: member("permission_token")?,
        result_class: operation
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("none")
            .to_owned(),
        input_fields: input_fields_of(&input),
        // A `.result.json` is the exception, not the rule: exactly one shipped
        // operation has one. Every generated CRUD operation declares a result
        // CLASS in its operation contract and describes the result's shape in
        // its SQL statement columns instead. Reading only `.result.json` would
        // leave the emitter with nothing to type for `get`, `query` and
        // `update` — so the columns are the fallback, and the contract is
        // honoured either way it chose to express itself.
        result_fields: result_fields(&result, &operation),
        server_owned_fields: string_list(
            input
                .get("server_owned_fields")
                .and_then(|owned| owned.get("fields")),
        ),
        envelope: input.get("envelope").cloned(),
        paging: paging_of(&input),
        errors: errors_of(parts.errors.as_ref()),
    }))
}

/// Result descriptors from a `.result.json` when present, else from the SQL
/// statement columns the operation contract carries.
fn result_fields(result: &Value, operation: &Value) -> Vec<FieldIr> {
    let declared = fields_of(result);
    if !declared.is_empty() {
        return declared;
    }
    let mut columns: Vec<FieldIr> = operation
        .get("statements")
        .and_then(Value::as_array)
        .map(|statements| {
            statements
                .iter()
                .filter_map(|statement| statement.get("columns")?.as_array())
                .flatten()
                .filter_map(|column| {
                    Some(FieldIr {
                        path: column.get("name")?.as_str()?.to_owned(),
                        type_name: column.get("type")?.as_str()?.to_owned(),
                        nullable: column
                            .get("nullable")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        values: Vec::new(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    columns.sort();
    columns.dedup();
    columns
}

/// Input descriptors from a command contract's `fields` array, else from the
/// scalar members a generated CRUD contract declares.
///
/// TWO SHAPES, both authored by the generator, and reading only one leaves
/// seven of the twelve shipped operations with no input at all. A COMMAND
/// contract (`receiving/record_receipt`) carries a `fields` array. A generated
/// CRUD contract (`purchase_order/get`) instead names each input as a
/// top-level member carrying `{required, type}` — `id`, `request_id`,
/// `expected_row_version` — and lists the three-state updatable ones under
/// `writable_fields`.
///
/// A writable field is NULLABLE in the descriptor sense: `omitted: unchanged`
/// means a caller may leave it out, which is exactly the optionality a control
/// or a request struct needs to model. Its `explicit_null` disposition is a
/// refusal rule, not a shape, and stays in the contract where it is enforced.
fn input_fields_of(contract: &Value) -> Vec<FieldIr> {
    let declared = fields_of(contract);
    if !declared.is_empty() {
        return declared;
    }
    let Some(members) = contract.as_object() else {
        return Vec::new();
    };
    let mut fields: Vec<FieldIr> = members
        .iter()
        .filter_map(|(name, member)| {
            Some(FieldIr {
                path: name.clone(),
                type_name: member.get("type")?.as_str()?.to_owned(),
                // A scalar input member declares `required`; absent reads as
                // optional, which is the safer direction — a client that sends
                // an optional field is refused by the contract, one that omits
                // a required field never builds.
                nullable: !member
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                values: string_list(member.get("values")),
            })
        })
        .collect();
    fields.extend(
        contract
            .get("writable_fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|field| {
                Some(FieldIr {
                    path: field.get("field")?.as_str()?.to_owned(),
                    type_name: field.get("type")?.as_str()?.to_owned(),
                    nullable: true,
                    values: string_list(field.get("values")),
                })
            }),
    );
    fields.sort();
    fields.dedup();
    fields
}

fn fields_of(contract: &Value) -> Vec<FieldIr> {
    let mut fields: Vec<FieldIr> = contract
        .get("fields")
        .and_then(Value::as_array)
        .map(|fields| {
            fields
                .iter()
                .filter_map(|field| {
                    Some(FieldIr {
                        path: field.get("path")?.as_str()?.to_owned(),
                        type_name: field.get("type")?.as_str()?.to_owned(),
                        nullable: field
                            .get("nullable")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        values: string_list(field.get("values")),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    fields.sort();
    fields.dedup();
    fields
}

fn paging_of(input: &Value) -> Option<PagingIr> {
    let filters = input.get("filters").and_then(Value::as_array);
    let pagination = input.get("pagination");
    if filters.is_none() && pagination.is_none() {
        return None;
    }
    let mut filters: Vec<FilterIr> = filters
        .map(|filters| {
            filters
                .iter()
                .filter_map(|filter| {
                    Some(FilterIr {
                        field: filter.get("field")?.as_str()?.to_owned(),
                        binding: filter.get("binding")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    filters.sort();
    filters.dedup();
    Some(PagingIr {
        filters,
        sort: input.get("sort").map(|sort| SortIr {
            fields: string_list(sort.get("fields")),
            directions: string_list(sort.get("directions")),
            max_fields: sort.get("max_fields").and_then(Value::as_u64),
        }),
        limit: input.get("limit").and_then(|limit| {
            Some(LimitIr {
                default: limit.get("default")?.as_u64()?,
                minimum: limit.get("minimum")?.as_u64()?,
                maximum: limit.get("maximum")?.as_u64()?,
            })
        }),
        pagination: pagination.cloned(),
    })
}

fn errors_of(errors: Option<&Value>) -> Vec<ErrorCaseIr> {
    let mut cases: Vec<ErrorCaseIr> = errors
        .and_then(|errors| errors.get("cases"))
        .and_then(Value::as_array)
        .map(|cases| {
            cases
                .iter()
                .filter_map(|case| {
                    let detail = case.get("detail");
                    Some(ErrorCaseIr {
                        literal: case.get("literal")?.as_str()?.to_owned(),
                        detail_required: string_list(detail.and_then(|d| d.get("required"))),
                        detail_optional: string_list(detail.and_then(|d| d.get("optional"))),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    cases.sort_by(|left, right| left.literal.cmp(&right.literal));
    cases.dedup_by(|left, right| left.literal == right.literal);
    cases
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    let mut list: Vec<String> = value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|entry| entry.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    list.sort();
    list.dedup();
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository_root() -> std::path::PathBuf {
        std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
            .expect("canonicalize repository root")
    }

    fn receiving_contracts() -> std::path::PathBuf {
        repository_root().join("packages/receiving/generated/contracts")
    }

    fn receiving_ir() -> ClientContractIr {
        ClientContractIr::from_contract_directory("receiving", &receiving_contracts())
            .expect("the shipped Receiving contract projection reads as an IR")
    }

    fn overlay_ir() -> ClientContractIr {
        ClientContractIr::from_contract_directory(
            "client_acme_receiving",
            &repository_root().join("packages/client_acme_receiving/generated/contracts"),
        )
        .expect("the shipped Acme overlay projection reads as an IR")
    }

    fn operation_names(ir: &ClientContractIr) -> Vec<String> {
        ir.models
            .iter()
            .flat_map(|model| {
                model
                    .operations
                    .iter()
                    .map(move |operation| format!("{}/{}", model.name, operation.name))
            })
            .collect()
    }

    fn operation<'a>(ir: &'a ClientContractIr, module: &str, name: &str) -> &'a OperationIr {
        ir.models
            .iter()
            .find(|model| model.name == module)
            .unwrap_or_else(|| panic!("no {module} model in {:?}", operation_names(ir)))
            .operations
            .iter()
            .find(|operation| operation.name == name)
            .unwrap_or_else(|| panic!("no {module}/{name} in {:?}", operation_names(ir)))
    }

    /// A private operation is not a client operation.
    ///
    /// The overlay's `quality/create_inspection` is an `event_handler` the
    /// platform invokes internally: `visibility: private`, null `grant`, null
    /// `permission_token`. Before this was excluded, projecting the overlay
    /// package failed outright — so this test also proves the overlay, the
    /// only package carrying the `/acme` routes, projects at all.
    #[test]
    fn a_private_operation_is_not_a_client_operation() {
        let names = operation_names(&overlay_ir());
        assert!(
            !names.iter().any(|name| name == "quality/create_inspection"),
            "a private event handler reached the client IR: {names:?}"
        );
        // Guard the guard: exclusion must remove ONE operation, not silence a
        // whole module. `create_inspection`'s siblings must survive.
        assert!(
            names
                .iter()
                .any(|name| name == "quality/approve_inspection"),
            "exclusion swallowed a callable sibling: {names:?}"
        );
        assert_eq!(names.len(), 5, "{names:?}");
    }

    /// Exclusion is by DECLARATION, never by a member that failed to parse.
    ///
    /// A public operation whose grant is missing is a malformed contract and
    /// must still refuse. Were the rule "skip anything without a grant", this
    /// contract would vanish from the client instead — a caller would find the
    /// operation simply absent, with nothing naming why.
    #[test]
    fn a_public_operation_missing_its_grant_still_refuses() {
        let scratch = std::env::temp_dir().join("wamn-client-ir-grantless");
        let _ = std::fs::remove_dir_all(&scratch);
        copy_tree(&receiving_contracts(), &scratch);
        let contract = scratch.join("purchase_order/get.operation.json");
        let mut document: Value =
            serde_json::from_slice(&std::fs::read(&contract).expect("read")).expect("parse");
        document
            .as_object_mut()
            .expect("an operation contract is an object")
            .remove("grant");
        std::fs::write(&contract, serde_json::to_vec(&document).expect("serialize"))
            .expect("write");

        let refusal = ClientContractIr::from_contract_directory("receiving", &scratch)
            .expect_err("a public operation with no grant is malformed");
        assert_eq!(refusal.kind, ClientIrErrorKind::MissingMember);
        assert!(refusal.to_string().contains("grant"), "{refusal}");
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// A generated CRUD contract states its inputs as scalar members, not as a
    /// `fields` array — and reading only the array left seven of the twelve
    /// shipped operations with no input at all.
    #[test]
    fn a_generated_crud_contract_yields_its_scalar_inputs() {
        let ir = receiving_ir();
        let update = operation(&ir, "purchase_order", "update");
        let paths: Vec<&str> = update
            .input_fields
            .iter()
            .map(|field| field.path.as_str())
            .collect();
        assert_eq!(
            paths,
            ["expected_row_version", "id", "request_id", "supplier_id"],
            "the three-state update's inputs"
        );

        let by_path = |name: &str| {
            update
                .input_fields
                .iter()
                .find(|field| field.path == name)
                .unwrap_or_else(|| panic!("no {name}"))
                .clone()
        };
        // Required scalars are not nullable; a writable field is, because
        // `omitted: unchanged` is exactly the optionality a caller models.
        assert_eq!(by_path("id").type_name, "uuid");
        assert!(!by_path("id").nullable);
        assert_eq!(by_path("expected_row_version").type_name, "int64");
        assert!(!by_path("expected_row_version").nullable);
        assert_eq!(by_path("supplier_id").type_name, "uuid");
        assert!(
            by_path("supplier_id").nullable,
            "a writable field is optional"
        );
    }

    /// The scalar fallback must not displace a command contract's own array.
    #[test]
    fn a_command_contract_still_reads_its_fields_array() {
        let ir = receiving_ir();
        let record = operation(&ir, "receiving", "record_receipt");
        assert_eq!(record.input_fields.len(), 8, "{:?}", record.input_fields);
        // A nested array path is the proof the ARRAY was read: the scalar
        // fallback walks top-level members and could never produce one.
        assert!(
            record
                .input_fields
                .iter()
                .any(|field| field.path == "value.line[].quantity"),
            "the command contract's own fields array was not read: {:?}",
            record.input_fields
        );
    }

    /// Every operation a client can see must carry inputs it can construct.
    /// This is the arithmetic the two shapes exist to satisfy; it caught the
    /// original hole and would catch either shape regressing.
    #[test]
    fn every_projected_operation_has_input_fields() {
        for ir in [receiving_ir(), overlay_ir()] {
            for model in &ir.models {
                for operation in &model.operations {
                    assert!(
                        !operation.input_fields.is_empty(),
                        "{}/{} projects no inputs",
                        model.name,
                        operation.name
                    );
                }
            }
        }
    }

    /// EXIT GATE 1a: regeneration is byte-identical on an unchanged release.
    #[test]
    fn regeneration_is_byte_identical_on_an_unchanged_release() {
        let first = receiving_ir().canonical_bytes();
        let second = receiving_ir().canonical_bytes();
        assert_eq!(first, second, "the IR is not byte-stable across two reads");
        assert!(
            !first.is_empty(),
            "an empty IR would make the equality above prove nothing"
        );
    }

    /// EXIT GATE 1a, THE REAL HALF: the IR is stable against INPUT REORDERING.
    ///
    /// Reading the same files twice is stable whether or not anything is
    /// normalized — the input order is identical both times, so that assertion
    /// alone passes even with every sort removed (measured: dropping the field
    /// sort leaves it green). Canonical means order-independent, so the gate
    /// has to reorder the input and demand the same bytes.
    #[test]
    fn the_ir_is_byte_identical_when_the_contract_members_are_reordered() {
        let scratch = std::env::temp_dir().join("wamn-client-ir-reordered");
        let _ = std::fs::remove_dir_all(&scratch);
        copy_tree(&receiving_contracts(), &scratch);
        let straight = ClientContractIr::from_contract_directory("receiving", &scratch)
            .expect("the copied projection reads")
            .canonical_bytes();

        // Reverse only the arrays whose order carries NO meaning — the field,
        // column, bind, case and filter lists the IR normalizes. Not every
        // array: `cursor.member_order` IS an ordering, and reversing it
        // genuinely changes the contract, so an IR that reported the same
        // bytes for it would be wrong. A canonical IR cannot notice the
        // former; an IR that merely echoes its input will.
        reverse_unordered_lists_in_tree(&scratch);
        let reordered = ClientContractIr::from_contract_directory("receiving", &scratch)
            .expect("the reordered projection reads")
            .canonical_bytes();

        assert_eq!(
            straight, reordered,
            "the IR changed when its input was reordered, so it is echoing order rather than \
             normalizing it"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Array members whose order the contract does not ascribe meaning to.
    const UNORDERED_LISTS: [&str; 6] = [
        "fields",
        "columns",
        "binds",
        "cases",
        "filters",
        "statements",
    ];

    fn reverse_unordered_lists_in_tree(directory: &Path) {
        for entry in std::fs::read_dir(directory)
            .expect("read scratch")
            .flatten()
        {
            let path = entry.path();
            if path.is_dir() {
                reverse_unordered_lists_in_tree(&path);
                continue;
            }
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(mut value) = serde_json::from_str::<Value>(&source) else {
                continue;
            };
            reverse_unordered_lists(&mut value);
            std::fs::write(&path, serde_json::to_string(&value).expect("serialize"))
                .expect("write reordered contract");
        }
    }

    fn reverse_unordered_lists(value: &mut Value) {
        if let Value::Object(members) = value {
            for (key, member) in members.iter_mut() {
                if UNORDERED_LISTS.contains(&key.as_str())
                    && let Value::Array(items) = member
                {
                    items.reverse();
                }
                reverse_unordered_lists(member);
            }
        }
    }

    /// The IR must actually carry the projection, not an empty shell that
    /// trivially round-trips. This is the guard on the gate above.
    #[test]
    fn the_ir_carries_the_shipped_projection() {
        let ir = receiving_ir();
        assert_eq!(ir.format_version, CLIENT_IR_FORMAT_VERSION);
        assert!(
            ir.cursor.is_some(),
            "the package pages, so it has a cursor contract"
        );

        let models: Vec<&str> = ir.models.iter().map(|model| model.name.as_str()).collect();
        assert!(
            models.contains(&"purchase_order") && models.contains(&"receiving"),
            "expected the shipped models, got {models:?}"
        );

        let purchase_order = ir
            .models
            .iter()
            .find(|model| model.name == "purchase_order")
            .expect("purchase_order is a shipped model");
        let operations: Vec<&str> = purchase_order
            .operations
            .iter()
            .map(|operation| operation.name.as_str())
            .collect();
        assert_eq!(operations, vec!["get", "query", "update"]);
    }

    /// EXIT GATE 1b: a contract change surfaces in the IR without hand edits.
    ///
    /// Driven by copying the real projection and editing ONE contract file, so
    /// the assertion is about the projection path rather than a synthetic
    /// fixture that could diverge from what ships.
    #[test]
    fn a_contract_change_surfaces_without_hand_edits() {
        let scratch = std::env::temp_dir().join("wamn-client-ir-contract-change");
        let _ = std::fs::remove_dir_all(&scratch);
        copy_tree(&receiving_contracts(), &scratch);

        let before = ClientContractIr::from_contract_directory("receiving", &scratch)
            .expect("the copied projection reads");

        // A new result field on one operation — the smallest contract change a
        // client would have to learn about.
        let result_path = scratch.join("receiving/record_receipt.result.json");
        let mut result: Value =
            serde_json::from_str(&std::fs::read_to_string(&result_path).expect("read result"))
                .expect("result is JSON");
        result["fields"]
            .as_array_mut()
            .expect("result fields are an array")
            .push(serde_json::json!({
                "path": "buyer_note",
                "type": "text",
                "nullable": true,
                "values": [],
            }));
        std::fs::write(
            &result_path,
            serde_json::to_string(&result).expect("serialize"),
        )
        .expect("write the changed contract");

        let after = ClientContractIr::from_contract_directory("receiving", &scratch)
            .expect("the changed projection reads");
        assert_ne!(
            before.canonical_bytes(),
            after.canonical_bytes(),
            "a new result field did not surface in the IR"
        );

        let model = after
            .models
            .iter()
            .find(|model| model.name == "receiving")
            .expect("receiving model");
        assert!(
            model.fields.iter().any(|field| field.path == "buyer_note"),
            "the new field reached the model descriptor set"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Field descriptors carry the closed value domain a control needs to
    /// render a choice rather than a free-text box.
    #[test]
    fn a_closed_value_domain_reaches_the_descriptor() {
        let ir = receiving_ir();
        let status = ir
            .models
            .iter()
            .flat_map(|model| model.fields.iter())
            .find(|field| field.path == "purchase_order_status")
            .expect("purchase_order_status ships with a closed domain");
        assert!(
            !status.values.is_empty(),
            "the closed domain was dropped from the descriptor: {status:?}"
        );
    }

    /// Permissions and coordinates are part of the IR, because a client that
    /// cannot name the grant cannot explain a 403.
    #[test]
    fn operations_carry_their_permission_and_identity() {
        let ir = receiving_ir();
        for model in &ir.models {
            for operation in &model.operations {
                assert!(!operation.permission_token.is_empty(), "{operation:?}");
                assert!(!operation.grant.is_empty(), "{operation:?}");
                assert!(operation.operation.contains(':'), "{operation:?}");
            }
        }
    }

    /// A paging operation carries its filters, limit and cursor contract; a
    /// command does not pretend to.
    #[test]
    fn paging_is_present_only_where_the_contract_declares_it() {
        let ir = receiving_ir();
        let purchase_order = ir
            .models
            .iter()
            .find(|model| model.name == "purchase_order")
            .expect("purchase_order model");
        let query = purchase_order
            .operations
            .iter()
            .find(|operation| operation.name == "query")
            .expect("query operation");
        let paging = query.paging.as_ref().expect("query pages");
        assert!(!paging.filters.is_empty(), "query declares filters");
        assert!(paging.pagination.is_some(), "query declares pagination");
        assert_eq!(query.result_class, "page");

        let get = purchase_order
            .operations
            .iter()
            .find(|operation| operation.name == "get")
            .expect("get operation");
        assert!(
            get.paging.is_none(),
            "a single-row read must not claim paging"
        );
    }

    /// Only ONE shipped operation has a `.result.json`; every generated CRUD
    /// operation describes its result through SQL statement columns instead.
    /// Without the fallback the emitter would have nothing to type for `get`,
    /// `query` or `update`, so this pins it.
    #[test]
    fn a_result_class_without_a_result_contract_still_types_its_rows() {
        let ir = receiving_ir();
        let purchase_order = ir
            .models
            .iter()
            .find(|model| model.name == "purchase_order")
            .expect("purchase_order model");

        for name in ["get", "query", "update"] {
            let operation = purchase_order
                .operations
                .iter()
                .find(|operation| operation.name == name)
                .unwrap_or_else(|| panic!("{name} is a shipped operation"));
            assert_ne!(
                operation.result_class, "none",
                "{name} declares a result class"
            );
            assert!(
                !operation.result_fields.is_empty(),
                "{name} declares a result class but the IR typed no rows for it"
            );
        }

        // And the fallback is not silently overriding a real result contract.
        let record_receipt = ir
            .models
            .iter()
            .find(|model| model.name == "receiving")
            .expect("receiving model")
            .operations
            .iter()
            .find(|operation| operation.name == "record_receipt")
            .expect("record_receipt operation");
        assert!(
            record_receipt
                .result_fields
                .iter()
                .any(|field| field.path == "purchase_order_status" && !field.values.is_empty()),
            "the declared result contract, with its closed domain, must win over columns"
        );
    }

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("create scratch");
        for entry in std::fs::read_dir(from).expect("read source").flatten() {
            let path = entry.path();
            let target = to.join(entry.file_name());
            if path.is_dir() {
                copy_tree(&path, &target);
            } else {
                std::fs::copy(&path, &target).expect("copy contract");
            }
        }
    }
}
