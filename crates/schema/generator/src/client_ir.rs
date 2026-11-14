//! Client fields and calls projected from compiled operation descriptions.

use std::collections::{BTreeMap, BTreeSet};

use wamn_record_history::{HISTORY_TABLE_SUFFIX, is_history_table_name};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client_fields::{fields_of, input_fields_of, schema_fields};

/// The input path that carries a page cursor.
///
/// This path and the four below it are fixed by the query input contract that
/// `generate/operations.rs` writes, so every consumer reads the same spelling.
pub const CURSOR_INPUT: &str = "cursor";

/// The input path that carries the page size.
pub const LIMIT_INPUT: &str = "limit";

/// The input path that carries the sort field.
pub const SORT_FIELD_INPUT: &str = "sort.field";

/// The input path that carries the sort direction.
pub const SORT_DIRECTION_INPUT: &str = "sort.direction";

/// The prefix of every filter input path.
pub const FILTER_PREFIX: &str = "filter.";

/// The input path that carries a server search.
pub const SEARCH_INPUT: &str = "search";

/// Why a contract projection could not be read as an IR.
#[derive(Debug)]
pub struct ClientIrError {
    type_: ClientIrErrorType,
    detail: String,
}

impl ClientIrError {
    /// Stable refusal class.
    #[must_use]
    pub const fn error_type(&self) -> ClientIrErrorType {
        self.type_
    }

    pub(super) fn new(type_: ClientIrErrorType, detail: impl Into<String>) -> Self {
        Self {
            type_,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for ClientIrError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.type_.code(), self.detail)
    }
}

impl std::error::Error for ClientIrError {}

/// Stable classification for an IR refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientIrErrorType {
    /// A contract file was not the JSON the projection declares.
    MalformedContract,
    /// A required contract member was absent.
    MissingMember,
    /// One operation is published at more than one route.
    AmbiguousRoute,
}

impl ClientIrErrorType {
    /// Stable wire code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MalformedContract => "malformed_contract",
            Self::MissingMember => "missing_member",
            Self::AmbiguousRoute => "ambiguous_route",
        }
    }
}

/// One package's client fields, held only in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientContractIr {
    /// The package this IR was projected from.
    pub package: String,
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
/// stamped in later at release publish from that flag. So the client's base URL
/// and host stay construction-time deployment config, and generated code
/// carries no deployment fact — not by our restraint, but because the fact is
/// structurally absent from what we read.
///
/// The template keeps its AUTHORED parameter names. This is deliberately NOT
/// [`wamn_catalog::exposure::canonical_http_route_template`]:
/// that collapses `{id}` to `{}` to build a route-COLLISION key, and a client
/// handed the collapsed form would have no name to substitute into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RouteIr {
    /// HTTP method, from the type of the operation the route calls: `GET` for
    /// a read, `POST` otherwise. No author writes it.
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
    /// The relations a direct route's read selects from. Empty for a write
    /// and for a composed route.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<String>,
    /// The relations a direct route's write changes, or `None` when the
    /// client cannot tell: a read, a composed route, or a contract that
    /// names no relation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writes: Option<Vec<String>>,
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
    /// Whether this signed integer is a revision serialized as a decimal JSON
    /// string, and the input path whose record it guards when the contract
    /// names one.
    #[serde(default, skip_serializing_if = "crate::manifest::Revision::is_absent")]
    pub revision: crate::manifest::Revision,
    /// Object members or repeated item members, ordered by path.
    pub children: Vec<FieldIr>,
    /// Declared repeated-item bounds.
    pub minimum: Option<u64>,
    pub maximum: Option<u64>,
    /// Closed value domain, when the contract declares one. Empty means open.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// Authored text, or the exact field path when no label is declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Authored selection for generated table and detail fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    /// Authored text for an author, which reaches a comment and no screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The record this input names, when it names one.
    ///
    /// An authored operation declares it. A generated action derives it from
    /// the column's own foreign key, so both arrive here the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub references: Option<ReferenceIr>,
    /// The model column this input writes, when a generated write states one
    /// (`writable_fields[].field`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
}

/// The model whose record one input names.
///
/// Read straight from the contract file, so its members keep the spelling
/// `generate/operations.rs` writes, exactly as [`RecordIr`] does.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceIr {
    /// Model whose record the value names.
    pub model: String,
    /// Input path of the same operation that narrows the list, when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrowed_by: Option<String>,
}

/// What one read operation lists: the key of each row, and the model whose
/// records the rows are, so a selector can offer them.
///
/// Read straight from the contract file, so its members keep the spelling
/// `generate/operations.rs` writes, exactly as [`RecordIr`] does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListIr {
    /// Model whose records a selector offers from these rows, absent when no
    /// selector offers them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Result fields whose values together name one row.
    #[serde(with = "crate::manifest::row_key")]
    pub key_field: Vec<String>,
    /// Result field that carries the text a person reads, when the author
    /// states one. Otherwise the plan displays the declared record key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_field: Option<String>,
}

impl FieldIr {
    /// Carry the declared facts of `source` onto every node of `fields` that
    /// states the same path: text, screen visibility, and the record it names.
    ///
    /// A served response reads its fields from the published route schema,
    /// which omits these facts. The declared contract of the
    /// responding terminal does, so the two join here rather than in the
    /// publication, and no attachment digest moves.
    pub fn carry_declared(fields: &mut [Self], source: &[Self]) {
        fn apply(fields: &mut [FieldIr], declared: &BTreeMap<&str, &FieldIr>) {
            for field in fields {
                if let Some(source) = declared.get(field.path.as_str()) {
                    if field.label.is_none() {
                        field.label.clone_from(&source.label);
                    }
                    if field.visible.is_none() {
                        field.visible = source.visible;
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

        fn all_fields(fields: &[FieldIr]) -> Vec<&FieldIr> {
            fields
                .iter()
                .flat_map(|field| {
                    let mut nodes = vec![field];
                    nodes.extend(all_fields(&field.children));
                    nodes
                })
                .collect()
        }
        let declared: BTreeMap<&str, &FieldIr> = all_fields(source)
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
    /// Operation type carried by the generated manifest contract.
    #[serde(rename = "type")]
    pub type_: String,
    /// Canonical operation identity.
    pub operation: String,
    /// The grant a caller needs.
    pub grant: String,
    /// Permission token this operation is authorized by.
    pub permission_token: String,
    /// Authored screen label, or the canonical operation identity when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Authored text for an author, which reaches a comment and no screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// What this read lists. Every read that a table shows states it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lists: Option<ListIr>,
    /// Whether the read's screen follows its feed through the sync store.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sync: bool,
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
    /// The relations a read selects from, as `schema.table`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reads: Vec<String>,
    /// The relations a write changes, as `schema.table`, or `None` when its
    /// contract names no relation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writes: Option<Vec<String>>,
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
        if field.revision.is_revision() {
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
    /// The text fields a server search reads, when the read declares one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search: Vec<String>,
    /// Sortable fields and permitted directions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<SortIr>,
    /// Limit bounds and default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<LimitIr>,
    /// Pagination type, cursor encoding, default sort and tie breaker. Left as
    /// contract JSON deliberately: its members carry semantic order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pagination: Option<Value>,
    /// Whether the read takes the feed part, which reads the changes since a
    /// cursor in place of a page.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub feed: bool,
}

/// One declared filter.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct FilterIr {
    /// The field this filter narrows.
    pub field: String,
    /// How the value binds, e.g. `json_array`.
    pub binding: String,
    /// How a value matches, when not exactly, e.g. `prefix` or `range`.
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_mode: Option<String>,
    /// The contract type of the field's values.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    /// Whether every read applies the filter: a band.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// The days a band reads when a request leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_last_days: Option<u64>,
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
    /// Authored text a screen shows in place of the code's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl ClientContractIr {
    /// Project compiled descriptions with the routes that serve them.
    pub fn from_operations(
        package: &str,
        operations: &BTreeMap<String, crate::OperationDescription>,
        routes: &BTreeMap<String, RouteIr>,
    ) -> Result<Self, ClientIrError> {
        let mut modules = BTreeMap::<String, BTreeMap<String, &crate::OperationDescription>>::new();
        for (local, operation) in operations {
            let (module, name) = local.split_once('.').expect("validated operation name");
            modules
                .entry(module.to_owned())
                .or_default()
                .insert(name.to_owned(), operation);
        }
        Self::assemble(package, modules, routes)
    }

    fn assemble(
        package: &str,
        modules: BTreeMap<String, BTreeMap<String, &crate::OperationDescription>>,
        routes: &BTreeMap<String, RouteIr>,
    ) -> Result<Self, ClientIrError> {
        let mut models = modules
            .into_iter()
            .map(|(name, operations)| build_model(&name, operations, routes))
            .collect::<Result<Vec<_>, _>>()?;
        bind_served_contracts(&mut models);
        for model in &mut models {
            default_field_labels(&mut model.fields);
            for operation in &mut model.operations {
                operation
                    .label
                    .get_or_insert_with(|| operation.operation.clone());
                default_field_labels(&mut operation.input_fields);
                default_field_labels(&mut operation.result_fields);
                if let Some(route) = &mut operation.route {
                    default_field_labels(&mut route.response.fields);
                }
            }
        }
        Ok(Self {
            package: package.to_owned(),
            models,
        })
    }
}

// Apply defaults after the declared and served shapes join, so a default
// cannot hide an authored label from the terminal operation.
fn default_field_labels(fields: &mut [FieldIr]) {
    for field in fields {
        field.label.get_or_insert_with(|| field.path.clone());
        default_field_labels(&mut field.children);
    }
}

/// Index final publication documents without resolving references.
pub fn published_routes(
    published: BTreeMap<String, wamn_catalog::ServingAttachment>,
    declarations: &Value,
) -> Result<BTreeMap<String, RouteIr>, ClientIrError> {
    let mut index = BTreeMap::<String, RouteIr>::new();
    for (id, attachment) in published {
        if attachment.type_ != wamn_catalog::AttachmentType::Http {
            continue;
        }
        let Some(operation) = &attachment.registered_operation else {
            continue;
        };
        let evidence = crate::client_route::evidence(&attachment, declarations)?;
        let template = attachment
            .definition
            .pointer("/route/path")
            .or_else(|| attachment.definition.pointer("/route/template"))
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ClientIrError::new(
                    ClientIrErrorType::MissingMember,
                    format!("attachment {id} has no route path"),
                )
            })?;
        let (fields, result_class) = if evidence.direct {
            (Vec::new(), None)
        } else {
            evidence
                .output_schema
                .as_ref()
                .map_or((Vec::new(), None), |schema| {
                    let value = schema.pointer("/items/properties/value").unwrap_or(schema);
                    let class = (value["type"] == "object").then(|| "one".to_owned());
                    (schema_fields(value, &[]), class)
                })
        };
        let route = RouteIr {
            method: String::new(),
            template: template.to_owned(),
            input_schema: evidence.input_schema,
            terminal_operation: evidence.terminal_operation,
            direct: evidence.direct,
            response: ResponseIr {
                schema: evidence.output_schema,
                partial_schema: evidence.partial_schema,
                fields,
                result_class,
                ..ResponseIr::default()
            },
            replay: None,
        };
        if let Some(existing) = index.insert(operation.clone(), route.clone())
            && existing != route
        {
            return Err(ClientIrError::new(
                ClientIrErrorType::AmbiguousRoute,
                format!("operation {operation} has more than one published route"),
            ));
        }
    }
    Ok(index)
}

fn build_model(
    name: &str,
    operations: BTreeMap<String, &crate::OperationDescription>,
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
                if existing.label.is_none() {
                    existing.label.clone_from(&field.label);
                }
                if existing.visible.is_none() {
                    existing.visible = field.visible;
                }
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

fn build_operation(
    module: &str,
    name: &str,
    parts: &crate::OperationDescription,
    routes: &BTreeMap<String, RouteIr>,
) -> Result<Option<OperationIr>, ClientIrError> {
    let operation = &parts.operation;
    if operation.visibility.as_deref() == Some("private") {
        return Ok(None);
    }
    let input = &parts.input;
    let result = &parts.result;
    let identity = operation.operation.clone();
    let type_ = operation.type_.as_str().to_owned();
    let record: Option<RecordIr> = operation
        .record
        .as_ref()
        .map(|record| serde_json::from_value(record.clone()))
        .transpose()
        .map_err(|error| {
            ClientIrError::new(ClientIrErrorType::MalformedContract, error.to_string())
        })?;
    let declared_fields = input_fields_of(input);
    let input_fields = routes
        .get(&identity)
        .and_then(|route| route.input_schema.as_ref())
        .map_or_else(
            || declared_fields.clone(),
            |schema| schema_fields(schema, &declared_fields),
        );
    let idempotent_by = operation.idempotent_by.clone();
    let (reads, writes) = relations_of(&type_, Some(&Value::Array(operation.relations.clone())));
    let requires_composition = matches!(type_.as_str(), "update" | "delete")
        || idempotent_by
            .as_ref()
            .is_some_and(|value| value.get("state").is_some())
        || leaf_fields(&input_fields)
            .iter()
            .any(|field| field.revision.is_revision());
    let route = routes.get(&identity).map(|route| RouteIr {
        method: operation.type_.http_method().to_owned(),
        ..route.clone()
    });
    Ok(Some(OperationIr {
        name: name.to_owned(),
        type_,
        route,
        record,
        revision_binding: None,
        requires_composition,
        idempotent_by,
        transaction: operation.transaction.clone(),
        operation: identity,
        grant: operation.grant.clone().ok_or_else(|| {
            ClientIrError::new(
                ClientIrErrorType::MissingMember,
                format!("{module}/{name} has no grant"),
            )
        })?,
        permission_token: operation.permission_token.clone().ok_or_else(|| {
            ClientIrError::new(
                ClientIrErrorType::MissingMember,
                format!("{module}/{name} has no permission token"),
            )
        })?,
        label: operation.label.clone(),
        description: operation.description.clone(),
        lists: operation
            .lists
            .as_ref()
            .map(|lists| serde_json::from_value(lists.clone()))
            .transpose()
            .map_err(|error| {
                ClientIrError::new(ClientIrErrorType::MalformedContract, error.to_string())
            })?,
        sync: operation.sync,
        result_class: operation
            .result
            .clone()
            .unwrap_or_else(|| "none".to_owned()),
        reads,
        writes,
        input_fields,
        result_fields: fields_of(result),
        server_owned_fields: string_list(
            input
                .get("server_owned_fields")
                .and_then(|owned| owned.get("fields")),
        ),
        envelope: input.get("envelope").cloned(),
        paging: paging_of(input),
        errors: errors_of(Some(&parts.errors)),
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
                route.response.reads.clone_from(&operation.reads);
                route.response.writes.clone_from(&operation.writes);
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
                if read.type_ != "get" {
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
    if filters.is_none() && pagination.is_none() && !input.get("feed").is_some_and(Value::is_object)
    {
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
                        match_mode: filter
                            .get("match")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        type_name: filter
                            .get("type")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        required: filter.get("required").and_then(Value::as_bool) == Some(true),
                        default_last_days: filter
                            .get("default")
                            .and_then(|default| default.get("last_days"))
                            .and_then(Value::as_u64),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    filters.sort();
    filters.dedup();
    Some(PagingIr {
        filters,
        search: input
            .get("search")
            .map(|search| string_list(search.get("fields")))
            .unwrap_or_default(),
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
        feed: input.get("feed").is_some_and(Value::is_object),
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
                        text: case.get("text").and_then(Value::as_str).map(str::to_owned),
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

/// The relations an operation reads and writes, from its contract's
/// `relations`, as `schema.table`. A read selects from every relation it
/// lists, and a history table is named as its model table, which changes in
/// the same transaction. A write changes each relation it inserts into,
/// updates or deletes from. A relation it only locks is not written.
fn relations_of(type_: &str, relations: Option<&Value>) -> (Vec<String>, Option<Vec<String>>) {
    let read = matches!(type_, "get" | "query" | "projection");
    let Some(relations) = relations.and_then(Value::as_array) else {
        return (Vec::new(), None);
    };
    let mut named = BTreeSet::new();
    for relation in relations {
        let (Some(schema), Some(table)) = (
            relation.get("schema").and_then(Value::as_str),
            relation.get("table").and_then(Value::as_str),
        ) else {
            continue;
        };
        let listed = |member: &str| {
            relation
                .get(member)
                .and_then(Value::as_array)
                .is_some_and(|fields| !fields.is_empty())
        };
        if read {
            let table = if is_history_table_name(table) {
                &table[..table.len() - HISTORY_TABLE_SUFFIX.len()]
            } else {
                table
            };
            named.insert(format!("{schema}.{table}"));
        } else if listed("insert_fields")
            || listed("update_fields")
            || relation.get("delete").and_then(Value::as_bool) == Some(true)
        {
            named.insert(format!("{schema}.{table}"));
        }
    }
    let named = named.into_iter().collect::<Vec<_>>();
    if read {
        (named, None)
    } else if relations.is_empty() {
        (Vec::new(), None)
    } else {
        (Vec::new(), Some(named))
    }
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
