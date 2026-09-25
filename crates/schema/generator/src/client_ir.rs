//! The canonical client-contract IR.
//!
//! ONE generator input: the effective-release contract projection under
//! `apps/<package>/generated/contracts/`. `wamn.json` is a contributor to
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

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client_fields::{fields_of, input_fields_of, schema_fields};

/// IR shape version. A consumer that does not recognise it must refuse rather
/// than guess at a field's meaning.
pub const CLIENT_IR_FORMAT_VERSION: u32 = 3;

/// The input path that carries a page cursor.
///
/// This path and the four below it are fixed by the query input contract that
/// `generate/contracts.rs` writes, so every consumer reads the same spelling.
pub const CURSOR_INPUT: &str = "cursor";

/// The input path that carries the page size.
pub const LIMIT_INPUT: &str = "limit";

/// The input path that carries the sort field.
pub const SORT_FIELD_INPUT: &str = "sort.field";

/// The input path that carries the sort direction.
pub const SORT_DIRECTION_INPUT: &str = "sort.direction";

/// The prefix of every filter input path.
pub const FILTER_PREFIX: &str = "filter.";

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

    pub(super) fn new(kind: ClientIrErrorKind, detail: impl Into<String>) -> Self {
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
    /// One operation is published at more than one route.
    AmbiguousRoute,
    /// A route is not in the form publication would normalize it to.
    UnnormalizedRoute,
}

impl ClientIrErrorKind {
    /// Stable wire code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnreadableProjection => "unreadable_projection",
            Self::MalformedContract => "malformed_contract",
            Self::MissingMember => "missing_member",
            Self::AmbiguousRoute => "ambiguous_route",
            Self::UnnormalizedRoute => "unnormalized_route",
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

/// Where one operation is published, as its release attached it.
///
/// METHOD AND TEMPLATE ONLY. The input this is read from cannot carry a host:
/// publication refuses one outright — `validate_authored_attachment_routes`
/// (`crates/control/lib/src/publish_release.rs:1293`) rejects an authored
/// `route.host` with "remove it and pass --route-host", and the host is
/// stamped in later at release mint from that flag. So the client's base URL
/// and host stay construction-time deployment config, and generated code
/// carries no deployment fact — not by our restraint, but because the fact is
/// structurally absent from what we read.
///
/// The template keeps its AUTHORED parameter names. This is deliberately NOT
/// `canonical_http_route_template` (`crates/schema/control/src/exposure.rs:351`):
/// that collapses `{id}` to `{}` to build a route-COLLISION key, and a client
/// handed the collapsed form would have no name to substitute into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RouteIr {
    /// HTTP method exactly as the attachment publishes it, e.g. `POST`.
    pub method: String,
    /// Authored path template, parameter names intact, e.g.
    /// `/purchase_order/{id}`.
    pub template: String,
    /// Declared input schema of the served route.
    pub input_schema: Option<Value>,
    /// Operation at the responding terminal, when the wiring declares one.
    pub terminal_operation: Option<String>,
    /// Whether the entire wiring is the registered operation alone.
    pub direct: bool,
    /// Contract of the responding terminal, never an arbitrary inner node.
    pub response: ResponseIr,
    /// Whole-submission replay guarantee. Unknown and composed routes have none.
    pub replay: Option<ReplayIr>,
}

/// The declared response of a served route.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ResponseIr {
    pub schema: Option<Value>,
    pub partial_schema: Option<Value>,
    pub result_class: Option<String>,
    pub fields: Vec<FieldIr>,
    pub errors: Vec<ErrorCaseIr>,
}

/// Replay guarantees declared by the served operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayIr {
    Claim,
    State,
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
    /// Whether the property must be present, independently of its null value.
    pub required: bool,
    /// Whether this signed integer is a revision serialized as a decimal JSON string.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub revision: bool,
    /// Input path whose record this revision guards, when the contract names
    /// one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision_of: Option<String>,
    /// Object members or repeated item members, ordered by path.
    pub children: Vec<FieldIr>,
    /// Declared repeated-item bounds.
    pub minimum: Option<u64>,
    pub maximum: Option<u64>,
    /// Closed value domain, when the contract declares one. Empty means open.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// Authored text a screen shows in place of the derived field name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Authored text for an author, which reaches a comment and no screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The record this input names, when it names one.
    ///
    /// An authored operation declares it. A generated action derives it from
    /// the column's own foreign key, so both arrive here the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<ReferenceIr>,
}

/// The model whose record one input names.
///
/// Read straight from the contract file, so its members keep the spelling
/// `generate/contracts.rs` writes, exactly as [`RecordIr`] does.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceIr {
    /// Model whose record the value names.
    pub model: String,
    /// Input path of the same operation that narrows the list, when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrowed_by: Option<String>,
}

/// What one read operation lists, so a selector can offer its rows.
///
/// Read straight from the contract file, so its members keep the spelling
/// `generate/contracts.rs` writes, exactly as [`RecordIr`] does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListIr {
    /// Model whose records the rows are.
    pub model: String,
    /// Result field that carries the record key.
    pub key_field: String,
    /// Result field that carries the text a person reads, when the author
    /// states one. The plan applies the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_field: Option<String>,
}

impl FieldIr {
    /// Carry the declared facts of `source` onto every leaf of `fields` that
    /// states the same path: the authored text and the record it names.
    ///
    /// A served response reads its fields from the published route schema,
    /// which carries neither by decision. The declared contract of the
    /// responding terminal does, so the two join here rather than in the
    /// publication, and no attachment digest moves.
    pub fn carry_declared(fields: &mut [Self], source: &[Self]) {
        fn apply(fields: &mut [FieldIr], declared: &BTreeMap<&str, &FieldIr>) {
            for field in fields {
                if let Some(source) = declared.get(field.path.as_str()) {
                    if field.label.is_none() {
                        field.label.clone_from(&source.label);
                    }
                    if field.description.is_none() {
                        field.description.clone_from(&source.description);
                    }
                    if field.references.is_none() {
                        field.references.clone_from(&source.references);
                    }
                }
                apply(&mut field.children, declared);
            }
        }

        let declared: BTreeMap<&str, &FieldIr> = leaf_fields(source)
            .into_iter()
            .map(|field| (field.path.as_str(), field))
            .collect();
        apply(fields, &declared);
    }
}

/// One operation, as a client must call it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct OperationIr {
    /// Local name within the model, e.g. `query`.
    pub name: String,
    /// Operation kind carried by the generated manifest contract.
    pub kind: String,
    /// Canonical operation identity.
    pub operation: String,
    /// The grant a caller needs.
    pub grant: String,
    /// Permission token this operation is authorized by.
    pub permission_token: String,
    /// Whether the registered operation requires a fresh originating credential.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fresh_only: bool,
    /// Authored name of the screen. A component exports it, and the page that
    /// places the component decides where the text goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Authored text for an author, which reaches a comment and no screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What this read lists, when a selector can offer its rows.
    ///
    /// A generated `query` states none: its `record` already names the
    /// relation and the key field that a row carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lists: Option<ListIr>,
    /// Where this operation is published, when the release exposes it over
    /// HTTP.
    ///
    /// Absent is a FACT, not a gap: an operation may be attached `internal` or
    /// `studio` rather than `http`, and a client that fabricated a path for one
    /// would call a route the deployment does not serve. No shipped package
    /// exercises the absent arm today — every callable operation in both
    /// packages is attached over HTTP.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<RouteIr>,
    /// Declared record mapping, absent for commands without a key binding.
    pub record: Option<RecordIr>,
    /// Compatible exposed record read and revision binding.
    pub revision_binding: Option<RevisionBindingIr>,
    /// A revision input cannot be submitted until Rust composition binds it.
    pub requires_composition: bool,
    /// The operation's own declaration; only the route grants safe replay.
    pub idempotent_by: Option<Value>,
    /// Declared transaction boundary, retained for submission evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction: Option<String>,
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

/// Record coordinates declared by a generated operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordIr {
    pub relation: String,
    pub key_field: String,
    pub key_input: Option<String>,
    pub revision_field: Option<String>,
    pub revision_input: Option<String>,
}

/// The read that supplies a command's record and expected revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RevisionBindingIr {
    pub read_operation: String,
    pub read_key_input: String,
    pub key_field: String,
    pub revision_field: String,
    pub command_key_input: String,
    pub command_revision_input: String,
}

/// Leaf descriptors for flat display controls and typed schema hints.
pub fn leaf_fields(fields: &[FieldIr]) -> Vec<&FieldIr> {
    fields
        .iter()
        .flat_map(|field| {
            if field.children.is_empty() {
                vec![field]
            } else {
                leaf_fields(&field.children)
            }
        })
        .collect()
}

/// Exact input paths that carry declared or platform-reserved revisions.
pub fn revision_inputs(operation: &OperationIr) -> Vec<&str> {
    let mut paths = BTreeSet::new();
    if let Some(path) = operation
        .record
        .as_ref()
        .and_then(|record| record.revision_input.as_deref())
    {
        paths.insert(path);
    }
    if let Some(guards) = operation
        .idempotent_by
        .as_ref()
        .and_then(|declaration| declaration.pointer("/state/guards"))
        .and_then(Value::as_object)
    {
        paths.extend(guards.values().filter_map(Value::as_str));
    }
    for field in leaf_fields(&operation.input_fields) {
        if field.revision {
            paths.insert(field.path.as_str());
        }
    }
    paths.into_iter().collect()
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
    /// Declared origins of this outcome, including ambiguous infrastructure failures.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
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
    /// Project one package's release into an IR: its contracts AND the routes
    /// its publication attaches them to.
    ///
    /// Two inputs, not one. Route templates are RELEASE facts — the base
    /// package publishes `wamn-receiving:purchase-order/get@1.0.0` at
    /// `/purchase_order/get` while the overlay publishes its own
    /// `client-acme-receiving:purchase-order/get@3.0.0` at
    /// `/acme/purchase_order/get` — so an IR built from contracts alone could
    /// only guess where an operation lives, and generated code that guessed
    /// would be wrong the first time it moved.
    ///
    /// # Errors
    ///
    /// [`ClientIrError`] naming the input that could not be read, the
    /// attachment whose route is malformed or un-normalized, or the operation
    /// published at more than one route.
    pub fn from_release(
        package: &str,
        contracts: &Path,
        attachments: &Path,
    ) -> Result<Self, ClientIrError> {
        // The document sits at `publication/attachments.json`, and the
        // generated schemas it names resolve against the package root above it.
        let root = attachments
            .parent()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new(""));
        let routes = route_index(attachments, &mut |reference| {
            crate::route_schema::read_from_package(root, reference)
        })?;
        Self::project(package, contracts, &routes)
    }

    /// Project a package with no published routes.
    ///
    /// Every operation's route is absent. Separate from [`Self::from_release`]
    /// rather than an `Option` argument, so "this release publishes nothing"
    /// and "I forgot to pass the attachments" can never look identical.
    ///
    /// # Errors
    ///
    /// [`ClientIrError`] naming the contract that could not be read.
    pub fn from_contract_directory(package: &str, contracts: &Path) -> Result<Self, ClientIrError> {
        Self::project(package, contracts, &BTreeMap::new())
    }

    fn project(
        package: &str,
        contracts: &Path,
        routes: &BTreeMap<String, RouteIr>,
    ) -> Result<Self, ClientIrError> {
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

        Self::assemble(package, modules, cursor, routes)
    }

    /// Project a release from contract files that are still in memory.
    ///
    /// The same projection as [`Self::from_release`], reading the bytes the
    /// generator has just produced instead of the copy on disk. Materialization
    /// emits the client beside the contracts in ONE pass, and it cannot read
    /// back a directory it has not written yet — nor should it, because the
    /// bytes it would read back are the ones it is about to overwrite.
    ///
    /// `contracts` is keyed by each file's path relative to
    /// `generated/contracts`, so `purchase_order/get.input.json` and
    /// `cursor-v1.json` are both addressed the way the directory addresses
    /// them. A path outside that shape is ignored, exactly as the directory
    /// walk ignores a file whose name it does not recognise.
    ///
    /// # Errors
    ///
    /// [`ClientIrError`] naming the contract that could not be read as JSON,
    /// the attachment whose route is malformed or un-normalized, or the
    /// operation published at more than one route.
    pub fn from_release_contracts(
        package: &str,
        contracts: &BTreeMap<String, Vec<u8>>,
        routes: &BTreeMap<String, RouteIr>,
    ) -> Result<Self, ClientIrError> {
        let mut modules: BTreeMap<String, BTreeMap<String, OperationParts>> = BTreeMap::new();
        let mut cursor = None;
        for (relative, bytes) in contracts {
            let parse = |bytes: &[u8]| -> Result<Value, ClientIrError> {
                serde_json::from_slice(bytes).map_err(|error| {
                    ClientIrError::new(
                        ClientIrErrorKind::MalformedContract,
                        format!("parse {relative}: {error}"),
                    )
                })
            };
            match relative.split_once('/') {
                Some((module, file_name)) => {
                    let Some((operation, part)) = split_contract_name(file_name) else {
                        continue;
                    };
                    let value = parse(bytes)?;
                    let slot = modules
                        .entry(module.to_owned())
                        .or_default()
                        .entry(operation.to_owned())
                        .or_default();
                    match part {
                        "operation" => slot.operation = Some(value),
                        "input" => slot.input = Some(value),
                        "result" => slot.result = Some(value),
                        "errors" => slot.errors = Some(value),
                        _ => {}
                    }
                }
                None if relative == "cursor-v1.json" => cursor = Some(parse(bytes)?),
                None => {}
            }
        }
        Self::assemble(package, modules, cursor, routes)
    }

    fn assemble(
        package: &str,
        modules: BTreeMap<String, BTreeMap<String, OperationParts>>,
        cursor: Option<Value>,
        routes: &BTreeMap<String, RouteIr>,
    ) -> Result<Self, ClientIrError> {
        let mut models = modules
            .into_iter()
            .map(|(name, operations)| build_model(&name, operations, routes))
            .collect::<Result<Vec<_>, _>>()?;
        bind_served_contracts(&mut models);
        Ok(Self {
            format_version: CLIENT_IR_FORMAT_VERSION,
            package: package.to_owned(),
            cursor,
            models,
        })
    }
}

/// Read one release's published routes, or none when it publishes nothing.
///
/// A package with no `publication/attachments.json` publishes no HTTP route at
/// all, which is a real state and not a missing input: the emitted client then
/// carries every operation's types and descriptors and no invoke function,
/// which is what [`ClientContractIr::from_contract_directory`] already means.
///
/// `read` returns the generated route schema a reference names.
///
/// # Errors
///
/// [`ClientIrError`] when the file exists and is not a serving attachment map,
/// or a schema it names cannot be read.
pub fn published_routes(
    attachments: &Path,
    read: &mut dyn FnMut(&str) -> Result<Value, crate::route_schema::RouteSchemaError>,
) -> Result<BTreeMap<String, RouteIr>, ClientIrError> {
    if attachments.exists() {
        route_index(attachments, read)
    } else {
        Ok(BTreeMap::new())
    }
}

/// Operation identity -> published route, from one release's attachment map.
///
/// The join key is the attachment's `registered-operation`, which is the same
/// string the operation contract carries as `operation` — read verbatim on
/// both sides, never reconstructed from package, module and action, because a
/// reconstruction is a second identity that can drift from the first.
///
/// # Normalization
///
/// This reads the AUTHORED publication input, which publication normalizes on
/// a copy downstream (`normalize_http_route`, uppercasing the method and
/// trimming the path). This layer is a sibling reader of those bytes, not a
/// consumer of the normalized output, so an authored `"post"` would reach a
/// generated client as `post` and call a method the deployment does not serve.
///
/// It cannot simply normalize: `normalize_http_route` lives in
/// `wamn-schema-control`, which DEPENDS on this crate, so reaching for it
/// would be a dependency cycle — and re-implementing it here would make a
/// second normalization authority, which is worse than either. So an
/// un-normalized route REFUSES by name, and the author is told to write the
/// form publication would produce. Fail-closed, one authority, no cycle.
fn route_index(
    attachments: &Path,
    read: &mut dyn FnMut(&str) -> Result<Value, crate::route_schema::RouteSchemaError>,
) -> Result<BTreeMap<String, RouteIr>, ClientIrError> {
    let mut published: BTreeMap<String, wamn_catalog::ServingAttachment> =
        serde_json::from_value(read_json(attachments)?).map_err(|error| {
            ClientIrError::new(
                ClientIrErrorKind::MalformedContract,
                format!(
                    "{} is not a serving attachment map: {error}",
                    attachments.display()
                ),
            )
        })?;
    crate::route_schema::resolve_attachments(&mut published, read).map_err(|error| {
        let cause = std::error::Error::source(&error)
            .map(|source| format!(": {source}"))
            .unwrap_or_default();
        ClientIrError::new(
            ClientIrErrorKind::UnreadableProjection,
            format!("{}: {error}{cause}", attachments.display()),
        )
    })?;

    let mut index: BTreeMap<String, RouteIr> = BTreeMap::new();
    for (id, attachment) in published {
        // Only `http` publishes a route a package client can call. `internal`
        // carries none by construction, and `studio` is the authoring surface
        // — a generated package client that acquired a studio path would call
        // a control-plane route it was never generated for.
        if attachment.kind != wamn_catalog::AttachmentKind::Http {
            continue;
        }
        // No registered operation means the attachment invokes no package
        // operation. Nothing to join to, and not an error.
        let Some(operation) = attachment.registered_operation.clone() else {
            continue;
        };
        let member = |key: &str| -> Result<String, ClientIrError> {
            attachment
                .definition
                .get("route")
                .and_then(|route| route.get(key))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    ClientIrError::new(
                        ClientIrErrorKind::MissingMember,
                        format!("http attachment {id:?} has no route {key:?}"),
                    )
                })
        };
        let evidence = crate::client_route::evidence(attachments, &attachment)?;
        let route = RouteIr {
            method: member("method")?,
            template: member("template").or_else(|_| member("path"))?,
            input_schema: evidence.input_schema,
            terminal_operation: evidence.terminal_operation,
            direct: evidence.direct,
            response: ResponseIr {
                fields: evidence
                    .output_schema
                    .as_ref()
                    .map(|schema| {
                        schema_fields(
                            schema.pointer("/items/properties/value").unwrap_or(schema),
                            &[],
                        )
                    })
                    .unwrap_or_default(),
                result_class: evidence
                    .output_schema
                    .as_ref()
                    .and_then(|schema| schema.pointer("/items/properties/value/type"))
                    .filter(|kind| *kind == "object")
                    .map(|_| "one".to_owned()),
                schema: evidence.output_schema,
                partial_schema: evidence.partial_schema,
                ..ResponseIr::default()
            },
            replay: None,
        };
        if route.method != route.method.to_ascii_uppercase()
            || route.template != normalized_template(&route.template)
        {
            return Err(ClientIrError::new(
                ClientIrErrorKind::UnnormalizedRoute,
                format!(
                    "attachment {id:?} publishes {} {:?}; author it as publication would \
                     normalize it, {} {:?}",
                    route.method,
                    route.template,
                    route.method.to_ascii_uppercase(),
                    normalized_template(&route.template),
                ),
            ));
        }
        if let Some(existing) = index.insert(operation.clone(), route.clone())
            && existing != route
        {
            // Publication keys route uniqueness on (template, method), never
            // on the operation, so one operation at two paths is a shape it
            // ACCEPTS. A client cannot carry two answers to "where is this",
            // and picking one silently would drop a published route, so this
            // refuses and the ambiguity is ruled rather than guessed.
            return Err(ClientIrError::new(
                ClientIrErrorKind::AmbiguousRoute,
                format!(
                    "operation {operation:?} is published at both {} {:?} and {} {:?}",
                    existing.method, existing.template, route.method, route.template
                ),
            ));
        }
    }
    Ok(index)
}

/// The path form publication would normalize to: no trailing slash below the
/// root. Used only to REPORT the expected form in a refusal — never to rewrite
/// a route, which would make this a second normalization authority.
fn normalized_template(template: &str) -> String {
    let trimmed = template.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
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
    routes: &BTreeMap<String, RouteIr>,
) -> Result<ModelIr, ClientIrError> {
    let mut built = Vec::with_capacity(operations.len());
    for (operation_name, parts) in operations {
        if let Some(operation) = build_operation(name, &operation_name, parts, routes)? {
            built.push(operation);
        }
    }
    // The model's descriptor set is the union of what its operations return.
    // Unioned rather than taken from one operation, because `get` and `query`
    // may each project a subset and a control needs the whole field.
    //
    // UNIONED BY PATH, and nullability WIDENS. One operation's result may
    // admit null where another's does not — `update` returns the row it did
    // not write as null while `get` always projects it — and a plain sort and
    // dedup keeps both, so a table over this model would render the same
    // column twice with contradictory nullability. A control must assume the
    // weaker guarantee: a field null in ANY projection can arrive null.
    let mut merged: BTreeMap<String, FieldIr> = BTreeMap::new();
    for field in built
        .iter()
        .flat_map(|operation| leaf_fields(&operation.result_fields))
    {
        merged
            .entry(field.path.clone())
            .and_modify(|existing| {
                existing.nullable |= field.nullable;
                existing.required &= field.required;
                // A closed domain stated anywhere is the model's domain; two
                // different closed domains for one path union rather than one
                // silently winning.
                for value in &field.values {
                    if !existing.values.contains(value) {
                        existing.values.push(value.clone());
                    }
                }
                existing.values.sort();
            })
            .or_insert_with(|| field.clone());
    }
    let fields: Vec<FieldIr> = merged.into_values().collect();
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
    routes: &BTreeMap<String, RouteIr>,
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
    let fresh_only = match operation.get("fresh_only") {
        None => false,
        Some(Value::Bool(value)) => *value,
        Some(_) => {
            return Err(ClientIrError::new(
                ClientIrErrorKind::MalformedContract,
                format!("{module}/{name} fresh_only must be a boolean"),
            ));
        }
    };
    if operation.get("visibility").and_then(Value::as_str) == Some("private") {
        if fresh_only {
            return Err(ClientIrError::new(
                ClientIrErrorKind::MalformedContract,
                format!("private operation {module}/{name} must not require a fresh credential"),
            ));
        }
        return Ok(None);
    }
    // A participant is a public, authorized component export, but it can run
    // only inside a host-selected base transaction. It has no direct client
    // route or operator screen.
    if operation.get("transaction").and_then(Value::as_str) == Some("participant") {
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

    // Authored text is optional at every carrier, so an absent member is the
    // ordinary case and never a malformed contract.
    let text = |key: &str| {
        operation
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };

    let input = parts
        .input
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let result = parts
        .result
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

    let identity = member("operation")?;
    let kind = member("kind")?;
    let record: Option<RecordIr> = operation
        .get("record")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| {
            ClientIrError::new(
                ClientIrErrorKind::MalformedContract,
                format!("{module}/{name} record: {error}"),
            )
        })?;
    let declared_fields = input_fields_of(&input);
    let input_fields = routes
        .get(&identity)
        .and_then(|route| route.input_schema.as_ref())
        .map_or_else(
            || declared_fields.clone(),
            |schema| schema_fields(schema, &declared_fields),
        );
    let idempotent_by = operation.get("idempotent_by").cloned();
    let requires_composition = matches!(kind.as_str(), "update" | "delete")
        || idempotent_by
            .as_ref()
            .is_some_and(|value| value.get("state").is_some())
        || leaf_fields(&input_fields)
            .iter()
            .any(|field| field.revision);
    Ok(Some(OperationIr {
        name: name.to_owned(),
        kind,
        route: routes.get(&identity).cloned(),
        record,
        revision_binding: None,
        requires_composition,
        idempotent_by,
        transaction: operation
            .get("transaction")
            .and_then(Value::as_str)
            .map(str::to_owned),
        operation: identity,
        grant: member("grant")?,
        permission_token: member("permission_token")?,
        fresh_only,
        label: text("label"),
        description: text("description"),
        lists: operation
            .get("lists")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                ClientIrError::new(
                    ClientIrErrorKind::MalformedContract,
                    format!("{module}/{name} lists: {error}"),
                )
            })?,
        result_class: operation
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("none")
            .to_owned(),
        input_fields,
        result_fields: fields_of(&result),
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

fn bind_served_contracts(models: &mut [ModelIr]) {
    let operations: BTreeMap<_, _> = models
        .iter()
        .flat_map(|model| &model.operations)
        .map(|operation| (operation.operation.clone(), operation.clone()))
        .collect();
    for operation in models.iter_mut().flat_map(|model| &mut model.operations) {
        if let Some(route) = &mut operation.route {
            if let Some(terminal) = route
                .terminal_operation
                .as_ref()
                .and_then(|id| operations.get(id))
            {
                if route.response.result_class.is_none() {
                    route.response.result_class = Some(terminal.result_class.clone());
                    route.response.fields = terminal.result_fields.clone();
                } else {
                    // The published schema states the shape and carries no
                    // authored text and no reference. The terminal's declared
                    // contract carries both, so the two join here.
                    FieldIr::carry_declared(&mut route.response.fields, &terminal.result_fields);
                }
                route.response.errors = terminal.errors.clone();
            }
            if route.direct {
                route.replay = match operation.idempotent_by.as_ref() {
                    Some(value) if value == "claim" => Some(ReplayIr::Claim),
                    Some(value) if value.get("state").is_some() => Some(ReplayIr::State),
                    _ => None,
                };
            }
        }
    }
    // Read bindings consume the served response after all terminal contracts
    // are resolved, never an inner operation's unserved result.
    let operations: Vec<_> = models
        .iter()
        .flat_map(|model| &model.operations)
        .cloned()
        .collect();
    for operation in models.iter_mut().flat_map(|model| &mut model.operations) {
        if !operation.requires_composition {
            continue;
        }
        let Some(record) = &operation.record else {
            continue;
        };
        let (Some(key_input), Some(revision_input), Some(revision_field)) = (
            &record.key_input,
            &record.revision_input,
            &record.revision_field,
        ) else {
            continue;
        };
        let command_fields = leaf_fields(&operation.input_fields);
        let Some(key) = command_fields.iter().find(|field| &field.path == key_input) else {
            continue;
        };
        let Some(revision) = command_fields
            .iter()
            .find(|field| &field.path == revision_input)
        else {
            continue;
        };
        let candidates = operations
            .iter()
            .filter_map(|read| {
                if read.kind != "get" {
                    return None;
                }
                let route = read.route.as_ref()?;
                if route.response.result_class.as_deref() != Some("one") {
                    return None;
                }
                let read_record = read.record.as_ref()?;
                if read_record.relation != record.relation
                    || read_record.key_field != record.key_field
                {
                    return None;
                }
                let read_key_input = read_record.key_input.as_ref()?;
                let input = leaf_fields(&read.input_fields);
                let result = leaf_fields(&route.response.fields);
                let compatible = |field: &&FieldIr, path: &str, ty: &str| {
                    field.path == path && field.type_name == ty && field.required && !field.nullable
                };
                if !input
                    .iter()
                    .any(|field| compatible(field, read_key_input, &key.type_name))
                    || !result
                        .iter()
                        .any(|field| compatible(field, &record.key_field, &key.type_name))
                    || !result
                        .iter()
                        .any(|field| compatible(field, revision_field, &revision.type_name))
                {
                    return None;
                }
                Some(RevisionBindingIr {
                    read_operation: read.operation.clone(),
                    read_key_input: read_key_input.clone(),
                    key_field: record.key_field.clone(),
                    revision_field: revision_field.clone(),
                    command_key_input: key_input.clone(),
                    command_revision_input: revision_input.clone(),
                })
            })
            .collect::<Vec<_>>();
        if candidates.len() == 1 {
            operation.revision_binding = candidates.into_iter().next();
            operation.requires_composition = false;
        }
    }
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
                        sources: case.get("from").and_then(Value::as_str).map_or_else(
                            || string_list(case.get("from")),
                            |source| vec![source.to_owned()],
                        ),
                        detail_required: string_list(detail.and_then(|d| d.get("required"))),
                        detail_optional: string_list(detail.and_then(|d| d.get("optional"))),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    cases.sort_by(|left, right| left.literal.cmp(&right.literal));
    // One literal can name several constraints, and only some of them name a
    // field. A key every case requires stays required, and a key that some
    // case states is optional, so the order of the cases changes nothing.
    cases.dedup_by(|case, kept| {
        if case.literal != kept.literal {
            return false;
        }
        let stated = [
            &kept.detail_required,
            &kept.detail_optional,
            &case.detail_required,
            &case.detail_optional,
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<BTreeSet<_>>();
        kept.detail_required
            .retain(|key| case.detail_required.contains(key));
        kept.detail_optional = stated
            .into_iter()
            .filter(|key| !kept.detail_required.contains(key))
            .collect();
        true
    });
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

    #[test]
    fn fresh_only_contract_policy_is_preserved_in_the_client_ir() {
        for policy in [None, Some(false), Some(true)] {
            let mut contract = serde_json::json!({
                "operation": "orders:widget/get@1.0.0",
                "kind": "get",
                "grant": "orders:widget/get@1.0.0",
                "permission_token": "widget.get"
            });
            if let Some(value) = policy {
                contract["fresh_only"] = serde_json::json!(value);
            }
            let operation = build_operation(
                "widget",
                "get",
                OperationParts {
                    operation: Some(contract),
                    ..Default::default()
                },
                &BTreeMap::new(),
            )
            .expect("public operation contract projects")
            .expect("public operation remains visible");
            assert_eq!(operation.fresh_only, policy.unwrap_or(false));
            let serialized = serde_json::to_value(&operation).unwrap();
            assert_eq!(
                serialized.get("fresh-only").cloned(),
                policy.filter(|value| *value).map(Value::Bool)
            );
        }
    }

    #[test]
    fn fresh_only_client_ir_refuses_private_and_malformed_contracts() {
        for contract in [
            serde_json::json!({"fresh_only": true, "visibility": "private"}),
            serde_json::json!({"fresh_only": null}),
            serde_json::json!({"fresh_only": "true"}),
            serde_json::json!({"fresh_only": 1}),
        ] {
            let error = build_operation(
                "widget",
                "get",
                OperationParts {
                    operation: Some(contract),
                    ..Default::default()
                },
                &BTreeMap::new(),
            )
            .expect_err("private or malformed freshness policy is refused");
            assert_eq!(error.kind(), ClientIrErrorKind::MalformedContract);
        }
    }
}
