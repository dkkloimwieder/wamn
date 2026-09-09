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

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client_fields::{fields_of, input_fields_of, schema_fields};

/// IR shape version. A consumer that does not recognise it must refuse rather
/// than guess at a field's meaning.
pub const CLIENT_IR_FORMAT_VERSION: u32 = 2;

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
/// (`services/ctl/src/publish_release.rs:1293`) rejects an authored
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
    /// Object members or repeated item members, ordered by path.
    pub children: Vec<FieldIr>,
    /// Declared repeated-item bounds.
    pub minimum: Option<u64>,
    pub maximum: Option<u64>,
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
    /// Operation kind carried by the generated manifest contract.
    pub kind: String,
    /// Canonical operation identity.
    pub operation: String,
    /// The grant a caller needs.
    pub grant: String,
    /// Permission token this operation is authorized by.
    pub permission_token: String,
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
        if matches!(
            field.path.as_str(),
            "expected_row_version" | "value.expected_row_version"
        ) {
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
        Self::project(package, contracts, &route_index(attachments)?)
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
/// # Errors
///
/// [`ClientIrError`] when the file exists and is not a serving attachment map.
pub fn published_routes(attachments: &Path) -> Result<BTreeMap<String, RouteIr>, ClientIrError> {
    if attachments.exists() {
        route_index(attachments)
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
fn route_index(attachments: &Path) -> Result<BTreeMap<String, RouteIr>, ClientIrError> {
    let published: BTreeMap<String, wamn_catalog::ServingAttachment> =
        serde_json::from_value(read_json(attachments)?).map_err(|error| {
            ClientIrError::new(
                ClientIrErrorKind::MalformedContract,
                format!(
                    "{} is not a serving attachment map: {error}",
                    attachments.display()
                ),
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
                    .map(|schema| schema_fields(schema, &[]))
                    .unwrap_or_default(),
                schema: evidence.output_schema,
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
        || leaf_fields(&input_fields).iter().any(|field| {
            matches!(
                field.path.as_str(),
                "expected_row_version" | "value.expected_row_version"
            )
        });
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
                route.response.result_class = Some(terminal.result_class.clone());
                route.response.fields = terminal.result_fields.clone();
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
        let leaves = leaf_fields(&update.input_fields);
        let paths: Vec<&str> = leaves.iter().map(|field| field.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "change.supplier_id",
                "expected_row_version",
                "id",
                "request_id"
            ],
            "the three-state update's inputs"
        );

        let by_path = |name: &str| {
            leaves
                .iter()
                .find(|field| field.path == name)
                .copied()
                .unwrap_or_else(|| panic!("no {name}"))
        };
        // Property omission and null values follow separate contract facts.
        assert_eq!(by_path("id").type_name, "uuid");
        assert!(!by_path("id").nullable);
        assert_eq!(by_path("expected_row_version").type_name, "int64");
        assert!(!by_path("expected_row_version").nullable);
        assert_eq!(by_path("change.supplier_id").type_name, "uuid");
        assert!(!by_path("change.supplier_id").required);
        assert!(
            !by_path("change.supplier_id").nullable,
            "explicit null is refused"
        );
    }

    /// The scalar fallback must not displace a command contract's own array.
    #[test]
    fn a_command_contract_still_reads_its_fields_array() {
        let ir = receiving_ir();
        let record = operation(&ir, "receiving", "record_receipt");
        let leaves = leaf_fields(&record.input_fields);
        assert_eq!(leaves.len(), 8, "{:?}", record.input_fields);
        // A nested array path is the proof the ARRAY was read: the scalar
        // fallback walks top-level members and could never produce one.
        assert!(
            leaves
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

    fn attachments(package: &str) -> std::path::PathBuf {
        repository_root().join(format!("packages/{package}/publication/attachments.json"))
    }

    fn released(package: &str) -> ClientContractIr {
        ClientContractIr::from_release(
            package,
            &repository_root().join(format!("packages/{package}/generated/contracts")),
            &attachments(package),
        )
        .unwrap_or_else(|error| panic!("{package} projects with its routes: {error}"))
    }

    /// EXIT GATE (.5.8): an operation's published route reaches the IR from the
    /// release, with its method and its authored template.
    #[test]
    fn an_operation_carries_the_route_its_release_publishes() {
        let ir = released("receiving");
        let get = operation(&ir, "purchase_order", "get");
        let route = get.route.as_ref().expect("the get is exposed");
        assert_eq!(route.method, "POST");
        assert_eq!(route.template, "/purchase_order/get");
    }

    /// The SAME module and action sit at different paths in different
    /// packages, which is why a route cannot be derived from the operation
    /// name and must come from the release.
    #[test]
    fn the_overlay_publishes_its_own_path_for_its_own_operation() {
        let base = released("receiving");
        let overlay = released("client_acme_receiving");

        let base_get = operation(&base, "purchase_order", "get");
        let overlay_get = operation(&overlay, "purchase_order", "get");

        assert_eq!(
            base_get.route.as_ref().map(|route| route.template.as_str()),
            Some("/purchase_order/get")
        );
        assert_eq!(
            overlay_get
                .route
                .as_ref()
                .map(|route| route.template.as_str()),
            Some("/acme/purchase_order/get")
        );
        // Same module and action, different operation identity AND different
        // path — a client that derived the path from the name would call the
        // base route from an overlay client.
        assert_ne!(base_get.operation, overlay_get.operation);
        assert_ne!(base_get.route, overlay_get.route);
    }

    /// Every callable operation in both shipped packages is published, and
    /// each carries the exact route its attachment declares. Arithmetic, so a
    /// route silently going missing cannot pass.
    #[test]
    fn every_shipped_operation_carries_its_published_route() {
        for package in ["receiving", "client_acme_receiving"] {
            let ir = released(package);
            let published: BTreeMap<String, (String, String)> =
                serde_json::from_value::<BTreeMap<String, wamn_catalog::ServingAttachment>>(
                    read_json(&attachments(package)).expect("attachments read"),
                )
                .expect("attachments decode")
                .into_values()
                .filter_map(|attachment| {
                    let route = attachment.definition.get("route")?;
                    Some((
                        attachment.registered_operation?,
                        (
                            route.get("method")?.as_str()?.to_owned(),
                            route.get("path")?.as_str()?.to_owned(),
                        ),
                    ))
                })
                .collect();

            for model in &ir.models {
                for operation in &model.operations {
                    let expected = published.get(&operation.operation).unwrap_or_else(|| {
                        panic!("{package}: {} is not published", operation.operation)
                    });
                    let route = operation.route.as_ref().unwrap_or_else(|| {
                        panic!("{package}: {} carries no route", operation.operation)
                    });
                    assert_eq!(
                        (route.method.as_str(), route.template.as_str()),
                        (expected.0.as_str(), expected.1.as_str()),
                        "{package}: {}",
                        operation.operation
                    );
                }
            }
        }
    }

    /// Without a release, every route is absent — and absent is stated, not
    /// invented.
    #[test]
    fn a_package_with_no_release_carries_no_routes() {
        let ir = receiving_ir();
        assert!(
            ir.models
                .iter()
                .flat_map(|model| &model.operations)
                .all(|operation| operation.route.is_none()),
            "a contracts-only projection invented a route"
        );
    }

    /// One operation published at two paths REFUSES. Publication keys route
    /// uniqueness on (template, method) and never on the operation, so this is
    /// a shape it accepts; a client cannot carry two answers, and silently
    /// taking one would drop a published route.
    #[test]
    fn an_operation_published_twice_refuses() {
        let scratch = std::env::temp_dir().join("wamn-client-ir-ambiguous-route");
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch");
        let file = scratch.join("attachments.json");

        let mut published: BTreeMap<String, Value> =
            serde_json::from_value(read_json(&attachments("receiving")).expect("read"))
                .expect("decode");
        let (_, mut alias) = published
            .iter()
            .next()
            .map(|(id, attachment)| (id.clone(), attachment.clone()))
            .expect("the shipped map is not empty");
        alias["definition"]["route"]["path"] = Value::String("/v2/purchase_order/get".to_owned());
        published.insert("receiving-purchase-order-get-v2".to_owned(), alias);
        std::fs::write(&file, serde_json::to_vec(&published).expect("serialize")).expect("write");

        let refusal = ClientContractIr::from_release("receiving", &receiving_contracts(), &file)
            .expect_err("one operation at two paths refuses");
        assert_eq!(refusal.kind, ClientIrErrorKind::AmbiguousRoute);
        assert!(
            refusal.to_string().contains("/v2/purchase_order/get"),
            "{refusal}"
        );
        let _ = std::fs::remove_dir_all(&scratch);
    }

    /// Build an attachment map from the shipped one with the first
    /// attachment's route rewritten. SYNTHETIC by necessity: no shipped route
    /// is parameterized and every shipped attachment is `http`, so the two
    /// properties below have no fixture and would otherwise go unproven —
    /// mutation testing confirmed both survived without this.
    fn attachments_with_first(
        directory: &str,
        edit: impl FnOnce(&mut Value),
    ) -> std::path::PathBuf {
        let scratch = std::env::temp_dir().join(directory);
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch");
        let file = scratch.join("attachments.json");

        let mut published: BTreeMap<String, Value> =
            serde_json::from_value(read_json(&attachments("receiving")).expect("read"))
                .expect("decode");
        let first = published.keys().next().cloned().expect("not empty");
        edit(published.get_mut(&first).expect("present"));
        std::fs::write(&file, serde_json::to_vec(&published).expect("serialize")).expect("write");
        file
    }

    /// A parameter name survives into the IR.
    ///
    /// This is the property that makes the route usable: a client substitutes
    /// BY NAME, so a template collapsed to the collision form
    /// (`canonical_http_route_template`) would leave it nothing to substitute
    /// into. No shipped route is parameterized, so the case is synthetic —
    /// but the platform's own tests author exactly this shape
    /// (`services/ctl/src/publish_release.rs:2496` sets `/receipt/{id}` on a
    /// serving attachment), so it is a real release shape, not an invented one.
    #[test]
    fn a_parameterized_template_keeps_its_parameter_name() {
        let file = attachments_with_first("wamn-client-ir-parameterized", |attachment| {
            attachment["definition"]["route"]["path"] =
                Value::String("/purchase_order/{id}".to_owned());
        });
        let ir = ClientContractIr::from_release("receiving", &receiving_contracts(), &file)
            .expect("a parameterized route projects");
        let templates: Vec<&str> = ir
            .models
            .iter()
            .flat_map(|model| &model.operations)
            .filter_map(|operation| operation.route.as_ref())
            .map(|route| route.template.as_str())
            .collect();
        assert!(
            templates.contains(&"/purchase_order/{id}"),
            "the parameter name was collapsed away: {templates:?}"
        );
        let _ = std::fs::remove_dir_all(file.parent().expect("scratch"));
    }

    /// Only `http` publishes a route a package client can call.
    ///
    /// `internal` carries none by construction, and `studio` is the authoring
    /// surface — a generated package client that acquired a studio path would
    /// call a control-plane route it was never generated for. Every shipped
    /// attachment is `http`, so this too is synthetic.
    #[test]
    fn a_studio_attachment_publishes_no_client_route() {
        let file = attachments_with_first("wamn-client-ir-studio", |attachment| {
            attachment["kind"] = Value::String("studio".to_owned());
        });
        let routed = |ir: &ClientContractIr| {
            ir.models
                .iter()
                .flat_map(|model| &model.operations)
                .filter(|operation| operation.route.is_some())
                .count()
        };
        let published = released("receiving");
        let ir = ClientContractIr::from_release("receiving", &receiving_contracts(), &file)
            .expect("a studio attachment projects");
        // The DELTA is the invariant, not a count: turning exactly one http
        // attachment into a studio one must remove exactly one client route,
        // and stays true as the package publishes more operations.
        assert_eq!(
            routed(&ir) + 1,
            routed(&published),
            "converting one attachment to studio did not remove exactly one route"
        );
        let _ = std::fs::remove_dir_all(file.parent().expect("scratch"));
    }

    /// An un-normalized route refuses rather than reaching a client. This
    /// layer reads the AUTHORED bytes, which publication normalizes only on a
    /// downstream copy, so a lowercase method would otherwise generate a
    /// client calling a method the deployment does not serve.
    #[test]
    fn an_unnormalized_route_refuses_and_names_the_form_publication_would_use() {
        let scratch = std::env::temp_dir().join("wamn-client-ir-unnormalized-route");
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).expect("scratch");
        let file = scratch.join("attachments.json");

        let mut published: BTreeMap<String, Value> =
            serde_json::from_value(read_json(&attachments("receiving")).expect("read"))
                .expect("decode");
        let first = published.keys().next().cloned().expect("not empty");
        published.get_mut(&first).expect("present")["definition"]["route"]["method"] =
            Value::String("post".to_owned());
        std::fs::write(&file, serde_json::to_vec(&published).expect("serialize")).expect("write");

        let refusal = ClientContractIr::from_release("receiving", &receiving_contracts(), &file)
            .expect_err("a lowercase method refuses");
        assert_eq!(refusal.kind, ClientIrErrorKind::UnnormalizedRoute);
        assert!(refusal.to_string().contains("POST"), "{refusal}");
        let _ = std::fs::remove_dir_all(&scratch);
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

    /// Every operation a client can call declares a result class, and the IR
    /// types its rows from the result CONTRACT — there is no columns fallback
    /// to fill the gap if the projection stops shipping one.
    #[test]
    fn every_result_class_is_typed_by_its_result_contract() {
        let ir = receiving_ir();
        for model in &ir.models {
            for operation in &model.operations {
                assert_ne!(
                    operation.result_class, "none",
                    "{}/{} declares a result class",
                    model.name, operation.name
                );
                assert!(
                    !operation.result_fields.is_empty(),
                    "{}/{} declares a result class but ships no result contract",
                    model.name,
                    operation.name
                );
            }
        }
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
