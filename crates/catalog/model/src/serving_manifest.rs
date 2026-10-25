//! The immutable release-serving manifest mounted by every serving process.
//!
//! Format 11 closes over exact package membership, component digests, routes,
//! the permission and SQL statements of each export, and route attachments.
//! Each component is its package, its admitted descriptor (the bytes of the
//! descriptor layer of its package artifact, docs/plan/platform-deploy.md
//! §7.2, contract A) and its released operations. The descriptor is the one
//! owner of the component name, interface version and digest. An
//! application's components compose at build into one component. A route
//! calls one component export. The workflow section holds the wiring definitions, the attachments
//! that start a wiring, and the registrations. A wiring is a graph the router
//! walks. It contains no flow or
//! execution-plan identity. Producers must source every member from current
//! catalog records; this model intentionally provides no legacy-plan
//! conversion. Attachment authentication uses a closed modes list, without a
//! scalar-mode fallback.
//!
//! The manifest names no tenant, environment, org, project, route host or
//! integer id, so one release hashes identically in every environment
//! (docs/plan/platform-deploy.md R1). A serving process takes its tenant and
//! environment from its own configuration.
//!
//! The document identity is the SHA-256 of its RFC 8785 canonical JSON. Sets and
//! maps make each collection's order deterministic, while
//! [`ServingManifest::from_canonical_bytes`] rejects bytes whose order or JSON
//! encoding differs from that canonical representation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AdmittedComponent, AdmittedComponentOperation, ArtifactHash, AttachmentType,
    CatalogIdentityError, ComponentConnection, ComponentDescriptor, ComponentSqlStatement,
    DefinitionHash, HASH_PREFIX, HostAttachment, HostRouteSet, ManifestDigest, PackageCoordinate,
    WiringDocument, package::validate_canonical_operation_for_package, validate_digest,
    validate_text,
};

/// The only serving-manifest format admitted by this revision.
pub const SERVING_MANIFEST_FORMAT_VERSION: u32 = 11;

/// The attachment auth-policy mode that permits an unauthenticated caller.
pub const NO_AUTHENTICATION_MODE: &str = "none";

/// The attachment auth-policy mode that requires a platform access token.
pub const PAT_AUTHENTICATION_MODE: &str = "pat";

/// The attachment auth-policy mode that permits a verified session token.
pub const SESSION_AUTHENTICATION_MODE: &str = "session";

/// The admitted authentication modes for one release attachment.
///
/// This parsed view is not a second wire representation. The manifest retains
/// the original JSON, including the canonical order of a combined modes list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentAuthPolicy<'a> {
    None,
    Pat,
    Session,
    PatAndSession,
    Signature {
        header: &'a str,
        credential: &'a str,
    },
}

impl AttachmentAuthPolicy<'_> {
    /// Whether this policy admits a request without a caller principal.
    pub fn has_no_caller(self) -> bool {
        matches!(self, Self::None | Self::Signature { .. })
    }

    /// Whether this attachment admits a platform access token.
    pub fn allows_pat(self) -> bool {
        matches!(self, Self::Pat | Self::PatAndSession)
    }

    /// Whether this attachment admits a verified session token.
    pub fn allows_session(self) -> bool {
        matches!(self, Self::Session | Self::PatAndSession)
    }
}

/// Parse the exact attachment authentication policy.
///
/// The only shapes are `{"modes":["none"]}`, `{"modes":["pat"]}`,
/// `{"modes":["session"]}`, `{"modes":["pat","session"]}`, and
/// `{"modes":["signature"],"header":"…","credential":"…"}`. Unknown
/// fields, scalar modes, duplicates, and other list orders are refused.
pub fn parse_attachment_auth_policy(policy: &Value) -> Option<AttachmentAuthPolicy<'_>> {
    let object = policy.as_object()?;
    let modes = object.get("modes")?.as_array()?;
    if matches!(modes.as_slice(), [mode] if mode.as_str() == Some("signature")) {
        if object.len() != 3 {
            return None;
        }
        let header = object.get("header")?.as_str()?;
        let credential = object.get("credential")?.as_str()?;
        return (!header.is_empty() && !credential.is_empty())
            .then_some(AttachmentAuthPolicy::Signature { header, credential });
    }
    if object.len() != 1 {
        return None;
    }
    match modes.as_slice() {
        [mode] => match mode.as_str()? {
            NO_AUTHENTICATION_MODE => Some(AttachmentAuthPolicy::None),
            PAT_AUTHENTICATION_MODE => Some(AttachmentAuthPolicy::Pat),
            SESSION_AUTHENTICATION_MODE => Some(AttachmentAuthPolicy::Session),
            _ => None,
        },
        [pat, session]
            if pat.as_str() == Some(PAT_AUTHENTICATION_MODE)
                && session.as_str() == Some(SESSION_AUTHENTICATION_MODE) =>
        {
            Some(AttachmentAuthPolicy::PatAndSession)
        }
        _ => None,
    }
}

/// Stable refusal literal for malformed or unsupported attachment auth policy.
pub const INVALID_ATTACHMENT_AUTH_POLICY_REFUSAL: &str = "invalid-attachment-auth-policy";

/// Stable refusal literal for a serving-manifest format this reader will not admit.
pub const UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL: &str =
    "unsupported-serving-manifest-version";

/// Name prefix of the immutable, digest-named ConfigMap carrying the manifest.
pub const RELEASE_MANIFEST_CONFIGMAP_PREFIX: &str = "release-manifest-";

/// Directory the manifest ConfigMap is projected into on every pod.
pub const RELEASE_MANIFEST_MOUNT_PATH: &str = "/etc/wamn/release-manifest";

/// The manifest ConfigMap's single key and mounted file name.
pub const RELEASE_MANIFEST_FILE_NAME: &str = "manifest.json";

/// Byte ceiling shared by the manifest publish and mount reader.
pub const MAX_SERVING_MANIFEST_BYTES: usize = 1024 * 1024;

/// The name of the ConfigMap carrying the manifest with this digest.
pub fn release_manifest_configmap_name(
    manifest_digest: &str,
) -> Result<String, CatalogIdentityError> {
    validate_digest(manifest_digest, "manifest-digest")?;
    let hex = manifest_digest
        .strip_prefix(HASH_PREFIX)
        .expect("a validated digest carries the sha256 prefix");
    Ok(format!("{RELEASE_MANIFEST_CONFIGMAP_PREFIX}{hex}"))
}

/// The exact package membership of the release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingRelease {
    pub packages: BTreeSet<PackageCoordinate>,
}

/// One immutable component artifact in the release closure.
///
/// One operation exported by a release component.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingComponentOperation {
    /// Explicit application permission identity. Palette exports carry none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_operation: Option<String>,
    /// The export's own registered operation. The caller must hold it.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub permissions: BTreeSet<String>,
    /// Canonical JSON for this registered operation's admitted committed result schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_result_schema: Option<String>,
    /// Exact SQL this export declares.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub statements: BTreeMap<String, ComponentSqlStatement>,
}

impl ServingComponentOperation {
    /// Resolve one release-pinned statement inside this operation's authority.
    pub fn statement(&self, path: &str) -> Option<&ComponentSqlStatement> {
        self.statements.get(path)
    }

    /// Whether this released operation carries the admitted operation's own facts.
    pub fn carries(&self, admitted: &AdmittedComponentOperation) -> bool {
        let schema = admitted.committed_result_schema.as_ref().map(|schema| {
            String::from_utf8(wamn_execution_contract::canonical_json_bytes(
                &schema.schema,
            ))
            .expect("canonical JSON uses UTF-8")
        });
        self.registered_operation == admitted.registered_operation
            && self.committed_result_schema == schema
            && admitted
                .registered_operation
                .as_ref()
                .is_none_or(|registered| self.permissions.contains(registered))
            && admitted
                .statements
                .iter()
                .all(|(path, statement)| self.statements.get(path) == Some(statement))
    }
}

/// One immutable component artifact in the release closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingComponent {
    pub package_id: String,
    /// The application schema selected at publication. Absent releases use the host fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_schema: Option<String>,
    /// The admitted descriptor of this component. Its canonical bytes are the
    /// descriptor layer of the package artifact that lists the component, so
    /// the release is the one owner of component facts (R2): the name, the
    /// interface version and the digest are read from here.
    pub descriptor: ComponentDescriptor,
    pub operations: BTreeMap<String, ServingComponentOperation>,
    /// The statement set of each named `statements` import of a composed
    /// component, keyed by import name: each member's own package's declared
    /// statements (Law 1 of `docs/plan/trx-pkg-plan.md`). Absent when empty, so
    /// a release without named instances keeps its bytes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub statement_instances: BTreeMap<String, BTreeMap<String, ComponentSqlStatement>>,
}

/// Components order by package, name, interface version, digest and
/// operations. A descriptor holds JSON values, which have no order, so the
/// order of two components with equal other fields is the order of their
/// descriptors' canonical bytes.
impl Ord for ServingComponent {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (
            &self.package_id,
            &self.runtime_schema,
            &self.descriptor.component,
            &self.descriptor.interface_version,
            &self.descriptor.component_digest,
            &self.operations,
            &self.statement_instances,
        )
            .cmp(&(
                &other.package_id,
                &other.runtime_schema,
                &other.descriptor.component,
                &other.descriptor.interface_version,
                &other.descriptor.component_digest,
                &other.operations,
                &other.statement_instances,
            ))
            .then_with(|| {
                let bytes = |descriptor: &ComponentDescriptor| {
                    wamn_execution_contract::canonical_json_bytes(
                        &serde_json::to_value(descriptor).expect("a descriptor serializes"),
                    )
                };
                bytes(&self.descriptor).cmp(&bytes(&other.descriptor))
            })
    }
}

impl PartialOrd for ServingComponent {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl ServingComponent {
    /// Project one admitted component.
    ///
    /// An application's components compose at build into one component, so a
    /// call to a base is a component-model call inside these bytes, with no
    /// host between. Every export carries its own permission and its own SQL
    /// statements; each member reaches its own statements through its named
    /// import ([`Self::with_statement_instances`]). The descriptor is `fact`
    /// with its `connections`.
    pub fn project(
        fact: &AdmittedComponent,
        connections: Vec<ComponentConnection>,
    ) -> Result<Self, CatalogIdentityError> {
        ArtifactHash::parse(fact.component_digest.clone())?;
        let mut operations = BTreeMap::new();
        for (name, operation) in &fact.operations {
            let released = ServingComponentOperation {
                registered_operation: operation.registered_operation.clone(),
                permissions: operation.registered_operation.iter().cloned().collect(),
                committed_result_schema: operation.committed_result_schema.as_ref().map(|schema| {
                    String::from_utf8(wamn_execution_contract::canonical_json_bytes(
                        &schema.schema,
                    ))
                    .expect("canonical JSON uses UTF-8")
                }),
                statements: operation.statements.clone(),
            };
            operations.insert(name.clone(), released);
        }
        Ok(Self {
            package_id: fact.scope.package_id.clone(),
            runtime_schema: None,
            descriptor: ComponentDescriptor::new(fact.clone(), connections),
            operations,
            statement_instances: BTreeMap::new(),
        })
    }

    /// Carry the verified statement instance record of a composed component
    /// and the statement set of each named import.
    ///
    /// `instances` maps each import name to its member's digest. A member
    /// whose digest `base` resolves is a base, and its set is the union of
    /// that base's own operations' statements. The one member that no base
    /// matches is the overlay, whose bytes are composed into this component,
    /// and its set is the union of this component's own operations'
    /// statements.
    ///
    /// # Errors
    ///
    /// Refuses two members that match no base, and one statement path with
    /// two different facts inside one package.
    pub fn with_statement_instances<'a>(
        mut self,
        instances: &BTreeMap<String, String>,
        base: &dyn Fn(&str) -> Option<&'a AdmittedComponent>,
    ) -> Result<Self, CatalogIdentityError> {
        let mut overlay = None;
        let mut sets = BTreeMap::new();
        for (name, digest) in instances {
            let operations = if let Some(base) = base(digest) {
                &base.operations
            } else {
                if let Some(previous) = overlay.replace(name) {
                    return invalid(format!(
                        "statement instances {previous:?} and {name:?} match no base"
                    ));
                }
                &self.descriptor.operations
            };
            sets.insert(name.clone(), own_statements(operations)?);
        }
        self.descriptor.statement_instances = instances.clone();
        self.statement_instances = sets;
        Ok(self)
    }
}

/// The union of the statements that `operations` declare.
fn own_statements(
    operations: &BTreeMap<String, AdmittedComponentOperation>,
) -> Result<BTreeMap<String, ComponentSqlStatement>, CatalogIdentityError> {
    let mut statements = BTreeMap::new();
    for (path, statement) in operations
        .values()
        .flat_map(|operation| &operation.statements)
    {
        if let Some(previous) = statements.insert(path.clone(), statement.clone())
            && &previous != statement
        {
            return invalid(format!(
                "statement {path:?} has two different facts in one package"
            ));
        }
    }
    Ok(statements)
}

/// One immutable wiring definition in the release closure: its package and
/// its canonical graph. The runtime walks this graph
/// (docs/plan/platform-deploy.md §5). The graph is the one carrier of the
/// wiring's id, version and hash; [`ServingWiring::identity`] derives them
/// (format 9, R2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingWiring {
    pub package_id: String,
    pub graph: WiringDocument,
}

impl ServingWiring {
    /// The wiring of `package_id` whose graph is `graph`.
    pub fn new(package_id: impl Into<String>, graph: WiringDocument) -> Self {
        Self {
            package_id: package_id.into(),
            graph,
        }
    }

    /// The wiring id, version and graph hash, derived from the graph.
    pub fn identity(&self) -> (&str, u32, DefinitionHash) {
        (
            &self.graph.wiring_id,
            self.graph.version,
            self.graph.wiring_hash(),
        )
    }

    fn key(&self) -> (&str, &str, u32, DefinitionHash) {
        let (wiring_id, wiring_version, graph_hash) = self.identity();
        (&self.package_id, wiring_id, wiring_version, graph_hash)
    }
}

/// The graph hash decides the graph, so the order is the order of the key.
impl Ord for ServingWiring {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key().cmp(&other.key())
    }
}

impl PartialOrd for ServingWiring {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// The contract kind of one application operation.
///
/// The values are the `kind` literals of the generated contract
/// `operation.json`, which is the only source of this fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationType {
    Get,
    Query,
    Create,
    Update,
    Delete,
    Command,
    Projection,
    EventHandler,
}

impl OperationType {
    /// The kinds that read without changing a record.
    pub const READ_KINDS: [Self; 3] = [Self::Get, Self::Query, Self::Projection];

    /// Whether this kind reads without changing a record.
    #[must_use]
    pub fn is_read(self) -> bool {
        Self::READ_KINDS.contains(&self)
    }

    /// The HTTP method of a route that calls an operation of this kind.
    ///
    /// A read is a GET and every other kind is a POST. The method follows from
    /// the kind alone, so no author writes it (`docs/architecture/execution.md`).
    #[must_use]
    pub fn http_method(self) -> &'static str {
        if self.is_read() { "GET" } else { "POST" }
    }
}

/// One component export that a route calls once, with no graph walk.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingRoute {
    pub package_id: String,
    pub component: String,
    pub operation: String,
    #[serde(rename = "type")]
    pub type_: OperationType,
    /// The model relations that a read of this route reads, from the
    /// `relations` of its generated contract. A history table stands for its
    /// model relation, whose version changes in the same transaction. A route
    /// of another kind reads none.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub reads: BTreeSet<ServingRelation>,
    /// The result field that carries the record revision of a `get`, from
    /// `record.revision_field` of its generated contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The input item field that carries the idempotency key, such as
    /// `value.idempotency_key`, from the generated input contract. A route
    /// without one keys its intents by the item `request_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency: Option<String>,
    /// The canonical form of an input item, from the generated input
    /// contract. A route without one takes its items as they are sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonicalization: Option<RouteCanonicalization>,
}

/// The canonical form of one route's input item
/// (`docs/history/host-transaction.md` 7, ruling (a)).
///
/// Two spellings of one request are one request: the host respells the
/// declared leaves and orders the declared lines before it hashes the item
/// and before the component gets it. Paths are dotted item paths, and `[]`
/// names each element of a list, such as `value.line[].quantity`.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RouteCanonicalization {
    /// Item paths that the request hash leaves out. The key and the
    /// `request_id` are never here.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub excluded: BTreeSet<String>,
    /// The line list and the member whose ascending order is its canonical
    /// order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<RouteLineOrder>,
    /// The leaves that take the canonical spelling of their type.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub spelling: BTreeMap<String, CanonicalSpelling>,
}

/// The line list of an item and the member that orders it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RouteLineOrder {
    /// The dotted path of the list, such as `value.line`.
    pub path: String,
    pub ascending_by: String,
}

/// The canonical spelling of one leaf type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CanonicalSpelling {
    /// PostgreSQL's own text of the value, scale kept.
    Numeric,
    /// UTC RFC 3339 with six fractional digits.
    Timestamptz,
}

impl RouteCanonicalization {
    /// Respell the declared leaves of `item` and order its declared lines.
    ///
    /// A value that the respelling refuses keeps its text, and the component
    /// refuses it.
    pub fn apply(&self, item: &mut Value) {
        for (path, spelling) in &self.spelling {
            let segments = path.split('.').collect::<Vec<_>>();
            respell(item, &segments, *spelling);
        }
        if let Some(order) = &self.lines
            && let Some(Value::Array(lines)) = order
                .path
                .split('.')
                .try_fold(&mut *item, |value, name| value.get_mut(name))
        {
            lines.sort_by_cached_key(|line| {
                line.get(&order.ascending_by)
                    .map_or_else(String::new, Value::to_string)
            });
        }
    }

    /// `item` without its excluded paths: what the request hash covers.
    #[must_use]
    pub fn hashed(&self, item: &Value) -> Value {
        let mut hashed = item.clone();
        for path in &self.excluded {
            let segments = path.split('.').collect::<Vec<_>>();
            remove(&mut hashed, &segments);
        }
        hashed
    }
}

/// Respell the leaves at `segments` below `value`.
fn respell(value: &mut Value, segments: &[&str], spelling: CanonicalSpelling) {
    let Some((first, rest)) = segments.split_first() else {
        if let Value::String(text) = value {
            let respelled = match spelling {
                CanonicalSpelling::Numeric => wamn_execution_contract::canonical_numeric(text),
                CanonicalSpelling::Timestamptz => {
                    wamn_execution_contract::canonical_timestamptz(text)
                }
            };
            if let Some(respelled) = respelled {
                *text = respelled;
            }
        }
        return;
    };
    match first.strip_suffix("[]") {
        Some(name) => {
            if let Some(Value::Array(elements)) = value.get_mut(name) {
                for element in elements {
                    respell(element, rest, spelling);
                }
            }
        }
        None => {
            if let Some(member) = value.get_mut(*first) {
                respell(member, rest, spelling);
            }
        }
    }
}

/// Remove the members at `segments` below `value`.
fn remove(value: &mut Value, segments: &[&str]) {
    match segments {
        [] => {}
        [last] => {
            if let Some(object) = value.as_object_mut() {
                object.remove(*last);
            }
        }
        [first, rest @ ..] => match first.strip_suffix("[]") {
            Some(name) => {
                if let Some(Value::Array(elements)) = value.get_mut(name) {
                    for element in elements {
                        remove(element, rest);
                    }
                }
            }
            None => {
                if let Some(member) = value.get_mut(*first) {
                    remove(member, rest);
                }
            }
        },
    }
}

/// One relation of a project database, by schema and name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingRelation {
    pub schema: String,
    pub relation: String,
}

/// What one release attachment invokes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentTarget {
    /// One component export of the attachment's package, through a route.
    Route {
        component: String,
        operation: String,
    },
    /// One exact wiring identity and version, walked by the router.
    Wiring {
        wiring_id: String,
        wiring_version: u32,
    },
}

/// One authored attachment targeting a route or an exact wiring version.
///
/// Publish reads this type from `publication/attachments.json` and splits it
/// into a [`RouteAttachment`] or a [`WiringAttachment`] of the manifest.
///
/// On the wire the target is flat: `component` and `operation`, or
/// `wiring-id` and `wiring-version`, never both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "AttachmentWire", into = "AttachmentWire")]
pub struct ServingAttachment {
    pub type_: AttachmentType,
    pub package_id: String,
    pub target: AttachmentTarget,
    pub definition_hash: DefinitionHash,
    pub definition: Value,
    pub auth_policy: Value,
    /// Exact operation authority selected by this attachment. Attachments that
    /// do not invoke a package operation carry no token; callers never infer one
    /// from route, wiring, or component syntax.
    pub registered_operation: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct AttachmentWire {
    #[serde(rename = "type")]
    type_: AttachmentType,
    package_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    component: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wiring_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wiring_version: Option<u32>,
    definition_hash: DefinitionHash,
    definition: Value,
    auth_policy: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    registered_operation: Option<String>,
}

impl TryFrom<AttachmentWire> for ServingAttachment {
    type Error = String;

    fn try_from(wire: AttachmentWire) -> Result<Self, Self::Error> {
        let target = match (
            wire.component,
            wire.operation,
            wire.wiring_id,
            wire.wiring_version,
        ) {
            (Some(component), Some(operation), None, None) => AttachmentTarget::Route {
                component,
                operation,
            },
            (None, None, Some(wiring_id), Some(wiring_version)) => AttachmentTarget::Wiring {
                wiring_id,
                wiring_version,
            },
            _ => {
                return Err(
                    "an attachment names exactly one target: component and operation, \
                     or wiring-id and wiring-version"
                        .to_owned(),
                );
            }
        };
        Ok(Self {
            type_: wire.type_,
            package_id: wire.package_id,
            target,
            definition_hash: wire.definition_hash,
            definition: wire.definition,
            auth_policy: wire.auth_policy,
            registered_operation: wire.registered_operation,
        })
    }
}

impl From<ServingAttachment> for AttachmentWire {
    fn from(attachment: ServingAttachment) -> Self {
        let (component, operation, wiring_id, wiring_version) = match attachment.target {
            AttachmentTarget::Route {
                component,
                operation,
            } => (Some(component), Some(operation), None, None),
            AttachmentTarget::Wiring {
                wiring_id,
                wiring_version,
            } => (None, None, Some(wiring_id), Some(wiring_version)),
        };
        Self {
            type_: attachment.type_,
            package_id: attachment.package_id,
            component,
            operation,
            wiring_id,
            wiring_version,
            definition_hash: attachment.definition_hash,
            definition: attachment.definition,
            auth_policy: attachment.auth_policy,
            registered_operation: attachment.registered_operation,
        }
    }
}

impl ServingAttachment {
    /// Split authored attachments into route attachments and workflow attachments.
    pub fn split(
        authored: BTreeMap<String, Self>,
    ) -> (
        BTreeMap<String, RouteAttachment>,
        BTreeMap<String, WiringAttachment>,
    ) {
        let mut routes = BTreeMap::new();
        let mut wirings = BTreeMap::new();
        for (id, attachment) in authored {
            match attachment.target {
                AttachmentTarget::Route {
                    component,
                    operation,
                } => {
                    routes.insert(
                        id,
                        RouteAttachment {
                            type_: attachment.type_,
                            package_id: attachment.package_id,
                            component,
                            operation,
                            definition_hash: attachment.definition_hash,
                            definition: attachment.definition,
                            auth_policy: attachment.auth_policy,
                            registered_operation: attachment.registered_operation,
                        },
                    );
                }
                AttachmentTarget::Wiring {
                    wiring_id,
                    wiring_version,
                } => {
                    wirings.insert(
                        id,
                        WiringAttachment {
                            type_: attachment.type_,
                            package_id: attachment.package_id,
                            wiring_id,
                            wiring_version,
                            definition_hash: attachment.definition_hash,
                            definition: attachment.definition,
                            auth_policy: attachment.auth_policy,
                            registered_operation: attachment.registered_operation,
                        },
                    );
                }
            }
        }
        (routes, wirings)
    }
}

impl From<RouteAttachment> for ServingAttachment {
    fn from(attachment: RouteAttachment) -> Self {
        Self {
            type_: attachment.type_,
            package_id: attachment.package_id,
            target: AttachmentTarget::Route {
                component: attachment.component,
                operation: attachment.operation,
            },
            definition_hash: attachment.definition_hash,
            definition: attachment.definition,
            auth_policy: attachment.auth_policy,
            registered_operation: attachment.registered_operation,
        }
    }
}

impl From<WiringAttachment> for ServingAttachment {
    fn from(attachment: WiringAttachment) -> Self {
        Self {
            type_: attachment.type_,
            package_id: attachment.package_id,
            target: AttachmentTarget::Wiring {
                wiring_id: attachment.wiring_id,
                wiring_version: attachment.wiring_version,
            },
            definition_hash: attachment.definition_hash,
            definition: attachment.definition,
            auth_policy: attachment.auth_policy,
            registered_operation: attachment.registered_operation,
        }
    }
}

impl From<AttachmentRef<'_>> for ServingAttachment {
    fn from(attachment: AttachmentRef<'_>) -> Self {
        match attachment {
            AttachmentRef::Route(attachment) => attachment.clone().into(),
            AttachmentRef::Wiring(attachment) => attachment.clone().into(),
        }
    }
}

/// One release attachment that calls one component export through a route.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RouteAttachment {
    #[serde(rename = "type")]
    pub type_: AttachmentType,
    pub package_id: String,
    pub component: String,
    pub operation: String,
    pub definition_hash: DefinitionHash,
    pub definition: Value,
    pub auth_policy: Value,
    /// Exact operation authority selected by this attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_operation: Option<String>,
}

/// One release attachment that starts an exact wiring version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WiringAttachment {
    #[serde(rename = "type")]
    pub type_: AttachmentType,
    pub package_id: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub definition_hash: DefinitionHash,
    pub definition: Value,
    pub auth_policy: Value,
    /// Exact operation authority selected by this attachment. Attachments that
    /// do not invoke a package operation carry no token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_operation: Option<String>,
}

/// One release attachment, borrowed from the route attachments or from the
/// workflow section.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AttachmentRef<'a> {
    Route(&'a RouteAttachment),
    Wiring(&'a WiringAttachment),
}

impl<'a> AttachmentRef<'a> {
    pub fn attachment_type(self) -> AttachmentType {
        match self {
            Self::Route(attachment) => attachment.type_,
            Self::Wiring(attachment) => attachment.type_,
        }
    }

    pub fn package_id(self) -> &'a str {
        match self {
            Self::Route(attachment) => &attachment.package_id,
            Self::Wiring(attachment) => &attachment.package_id,
        }
    }

    pub fn definition(self) -> &'a Value {
        match self {
            Self::Route(attachment) => &attachment.definition,
            Self::Wiring(attachment) => &attachment.definition,
        }
    }

    pub fn auth_policy(self) -> &'a Value {
        match self {
            Self::Route(attachment) => &attachment.auth_policy,
            Self::Wiring(attachment) => &attachment.auth_policy,
        }
    }

    pub fn registered_operation(self) -> Option<&'a str> {
        match self {
            Self::Route(attachment) => attachment.registered_operation.as_deref(),
            Self::Wiring(attachment) => attachment.registered_operation.as_deref(),
        }
    }

    /// The route or the wiring that this attachment starts.
    pub fn target(self) -> AttachmentTarget {
        match self {
            Self::Route(attachment) => AttachmentTarget::Route {
                component: attachment.component.clone(),
                operation: attachment.operation.clone(),
            },
            Self::Wiring(attachment) => AttachmentTarget::Wiring {
                wiring_id: attachment.wiring_id.clone(),
                wiring_version: attachment.wiring_version,
            },
        }
    }
}

/// One attachment a release serves: authored by a package, or a fixed host
/// route of the platform.
#[derive(Debug, Clone, Copy)]
pub enum ServedAttachment<'a> {
    Authored(AttachmentRef<'a>),
    Host(&'static HostAttachment),
}

impl<'a> ServedAttachment<'a> {
    pub fn attachment_type(self) -> AttachmentType {
        match self {
            Self::Authored(attachment) => attachment.attachment_type(),
            Self::Host(attachment) => attachment.attachment_type(),
        }
    }

    pub fn definition(self) -> &'a Value {
        match self {
            Self::Authored(attachment) => attachment.definition(),
            Self::Host(attachment) => &attachment.definition,
        }
    }

    pub fn auth_policy(self) -> &'a Value {
        match self {
            Self::Authored(attachment) => attachment.auth_policy(),
            Self::Host(attachment) => &attachment.auth_policy,
        }
    }

    pub fn registered_operation(self) -> Option<&'a str> {
        match self {
            Self::Authored(attachment) => attachment.registered_operation(),
            Self::Host(attachment) => attachment.registered_operation(),
        }
    }
}

/// The workflow facts of one release: what `wamn-workflow` walks and delivers.
///
/// A release with no wiring has an empty section, and the manifest omits it.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WorkflowSection {
    pub wirings: BTreeSet<ServingWiring>,
    /// The attachments that start a wiring.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attachments: BTreeMap<String, WiringAttachment>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub registrations: BTreeMap<String, ServingRegistration>,
}

impl WorkflowSection {
    /// Whether this release has no workflow fact.
    pub fn is_empty(&self) -> bool {
        self.wirings.is_empty() && self.attachments.is_empty() && self.registrations.is_empty()
    }
}

/// The delivery grain frozen for one release registration.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServingRegistrationInput {
    #[default]
    Event,
    Batch,
}

/// What the driver does with a delivery for one registration.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RegistrationDelivery {
    /// Start a workflow run through the queue: a declared workflow
    /// (docs/plan/workflow-feature.md 4.2).
    #[default]
    Queue,
}

/// Serde `skip_serializing_if` predicate for a field whose default carries no
/// information on the wire.
fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// One event registration targeting an exact wiring identity and version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingRegistration {
    /// Package that owns the target wiring and registration definition.
    pub package_id: String,
    /// Package whose committed change emits the event this registration reads.
    pub source_package_id: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub entity: String,
    pub ops: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub input: ServingRegistrationInput,
    /// The retained manifest delivery marker. Event starts use workflow rows.
    #[serde(default, skip_serializing_if = "is_default")]
    pub delivery: RegistrationDelivery,
}

/// Parse a workflow's five-field UTC cron expression.
///
/// # Errors
/// Returns an invalid-definition error for any other schedule syntax.
pub fn parse_workflow_schedule(text: &str) -> Result<croner::Cron, CatalogIdentityError> {
    if text.split_whitespace().count() != 5 {
        return invalid("workflow-schedule-invalid: requires five-field cron in UTC");
    }
    croner::parser::CronParser::builder()
        .seconds(croner::parser::Seconds::Disallowed)
        .year(croner::parser::Year::Disallowed)
        .build()
        .parse(text)
        .map_err(|error| CatalogIdentityError::InvalidDefinition {
            message: format!("workflow-schedule-invalid: {error}"),
        })
}

/// An application member whose exact bytes participate in the HTTP entry.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingEntryMember {
    pub package_id: String,
    pub component: String,
    pub digest: String,
}

/// The published HTTP component and the source identities that fix its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingComposedEntry {
    /// `entries@sha256:<manifest digest>`, relative to the component artifact
    /// base that each host resolves members against, or a local absolute `file:`.
    pub reference: String,
    pub route_digest: String,
    pub members: BTreeSet<ServingEntryMember>,
}

/// The complete release document a serving process mounts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingManifest {
    pub format_version: u32,
    pub release: ServingRelease,
    pub components: BTreeSet<ServingComponent>,
    /// Absent on roots that serve only host operations or have no HTTP entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composed_entry: Option<ServingComposedEntry>,
    pub routes: BTreeSet<ServingRoute>,
    /// The attachments that call a route.
    pub attachments: BTreeMap<String, RouteAttachment>,
    #[serde(default, skip_serializing_if = "WorkflowSection::is_empty")]
    pub workflow: WorkflowSection,
    /// The fixed host route sets this release serves
    /// (docs/plan/platform-ui.md §4.2). The platform builds their routes, so
    /// the manifest names only the sets.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub host_routes: BTreeSet<HostRouteSet>,
}

impl ServingManifest {
    /// Build and validate a manifest from authoritative current-record facts.
    ///
    /// The attachments are the authored ones, and this helper splits them the
    /// way publish does. This helper is test-only. The production publish
    /// serializes its projected facts and admits those exact bytes through
    /// [`Self::from_canonical_bytes`].
    #[cfg(feature = "test-util")]
    pub fn new(
        release: ServingRelease,
        components: BTreeSet<ServingComponent>,
        routes: BTreeSet<ServingRoute>,
        wirings: BTreeSet<ServingWiring>,
        attachments: BTreeMap<String, ServingAttachment>,
        registrations: BTreeMap<String, ServingRegistration>,
    ) -> Result<Self, CatalogIdentityError> {
        let (attachments, workflow_attachments) = ServingAttachment::split(attachments);
        let manifest = Self {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release,
            components,
            composed_entry: None,
            routes,
            attachments,
            workflow: WorkflowSection {
                wirings,
                attachments: workflow_attachments,
                registrations,
            },
            host_routes: BTreeSet::new(),
        };
        manifest.validate()?;
        within_delivery_limit(manifest.canonical_bytes().len())?;
        Ok(manifest)
    }

    /// One attachment by id, from the route attachments or the workflow section.
    pub fn attachment(&self, id: &str) -> Option<AttachmentRef<'_>> {
        self.attachments
            .get(id)
            .map(AttachmentRef::Route)
            .or_else(|| self.workflow.attachments.get(id).map(AttachmentRef::Wiring))
    }

    /// The members of the composed entry, in member order. Empty without an
    /// entry.
    pub fn entry_members(&self) -> Vec<&ServingComponent> {
        let Some(entry) = &self.composed_entry else {
            return Vec::new();
        };
        entry
            .members
            .iter()
            .filter_map(|member| {
                self.components.iter().find(|component| {
                    component.package_id == member.package_id
                        && component.descriptor.component == member.component
                        && component.descriptor.component_digest == member.digest
                })
            })
            .collect()
    }

    /// The member of the composed entry that composes the others. Its
    /// descriptor binds every nested member through a named SQL import, so it
    /// is the member with the most statement instances.
    pub fn composing_member(&self) -> Option<&ServingComponent> {
        self.entry_members()
            .into_iter()
            .max_by_key(|component| component.statement_instances.len())
    }

    /// The package id of the application that runs `component`: the
    /// composing member's package when `component` is a member of the composed
    /// entry, and `component`'s own package otherwise. One application is one
    /// composed workload, so a base operation reached through the entry runs
    /// as the composing application.
    pub fn composing_application<'a>(&'a self, component: &'a ServingComponent) -> &'a str {
        if self.entry_members().contains(&component)
            && let Some(composing) = self.composing_member()
        {
            return &composing.package_id;
        }
        &component.package_id
    }

    /// The route that calls `operation` of `component` in package `package_id`.
    pub fn route(
        &self,
        package_id: &str,
        component: &str,
        operation: &str,
    ) -> Option<&ServingRoute> {
        self.routes.iter().find(|route| {
            route.package_id == package_id
                && route.component == component
                && route.operation == operation
        })
    }

    /// Every attachment of the release: the route attachments, then the
    /// workflow section's. An id occurs once across both.
    pub fn every_attachment(&self) -> impl Iterator<Item = (&str, AttachmentRef<'_>)> {
        let routes = self
            .attachments
            .iter()
            .map(|(id, attachment)| (id.as_str(), AttachmentRef::Route(attachment)));
        let wirings = self
            .workflow
            .attachments
            .iter()
            .map(|(id, attachment)| (id.as_str(), AttachmentRef::Wiring(attachment)));
        routes.chain(wirings)
    }

    /// One host route of a set this release serves, by attachment id.
    pub fn host_attachment(&self, id: &str) -> Option<&'static HostAttachment> {
        self.host_routes.iter().find_map(|set| set.attachment(id))
    }

    /// One attachment that this release serves: an authored attachment or
    /// a host route.
    pub fn served_attachment(&self, id: &str) -> Option<ServedAttachment<'_>> {
        self.attachment(id)
            .map(ServedAttachment::Authored)
            .or_else(|| self.host_attachment(id).map(ServedAttachment::Host))
    }

    /// Every attachment this release serves: the authored attachments, then
    /// the host routes of its sets.
    pub fn every_served_attachment(&self) -> impl Iterator<Item = (&str, ServedAttachment<'_>)> {
        let authored = self
            .every_attachment()
            .map(|(id, attachment)| (id, ServedAttachment::Authored(attachment)));
        let host = self.host_routes.iter().flat_map(|set| {
            set.attachments()
                .map(|(id, attachment)| (id, ServedAttachment::Host(attachment)))
        });
        authored.chain(host)
    }

    /// The RFC 8785 canonical bytes mounted by serving processes.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        wamn_execution_contract::canonical_json_bytes(&self.as_value())
    }

    /// The content digest over [`Self::canonical_bytes`].
    pub fn digest(&self) -> ManifestDigest {
        ManifestDigest::parse(wamn_execution_contract::canonical_json_sha256(
            &self.as_value(),
        ))
        .expect("the shared canonicalizer emits a canonical sha256 digest")
    }

    /// Parse, validate, and admit only canonical format-11 bytes.
    ///
    /// The version is classified before the format-11 schema is decoded. This is
    /// what makes an unsupported mount an explicit typed refusal rather than a
    /// generic unknown-field parse error, and it deliberately provides no
    /// dual-version tolerance.
    pub fn from_canonical_bytes(
        bytes: &[u8],
    ) -> Result<(Self, ManifestDigest), CatalogIdentityError> {
        within_delivery_limit(bytes.len())?;
        let document = serde_json::from_slice::<Value>(bytes).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("serving manifest JSON is invalid: {error}"),
            }
        })?;
        validate_format_version(&document)?;
        // A release published before the schedules left the manifest
        // (docs/plan/workflows.md epic 2) may carry `workflow.schedules`. It
        // is read with the field ignored; its bytes and digest stay the ones
        // stored.
        let mut current = document.clone();
        if let Some(workflow) = current.get_mut("workflow").and_then(Value::as_object_mut) {
            workflow.remove("schedules");
        }
        let manifest = serde_json::from_value::<Self>(current.clone()).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("serving manifest JSON is invalid: {error}"),
            }
        })?;
        manifest.validate()?;
        if manifest.as_value() != current
            || wamn_execution_contract::canonical_json_bytes(&document) != bytes
        {
            return Err(CatalogIdentityError::NonCanonicalJson);
        }
        let digest =
            ManifestDigest::parse(wamn_execution_contract::canonical_json_sha256(&document))
                .expect("the shared canonicalizer emits a canonical sha256 digest");
        Ok((manifest, digest))
    }

    fn as_value(&self) -> Value {
        serde_json::to_value(self).expect("serving manifest serializes")
    }

    fn validate(&self) -> Result<(), CatalogIdentityError> {
        if self.format_version != SERVING_MANIFEST_FORMAT_VERSION {
            return Err(CatalogIdentityError::UnsupportedServingManifestVersion {
                requested: self.format_version.to_string(),
            });
        }
        if self.release.packages.is_empty() {
            return invalid("an effective release must contain at least one exact package pair");
        }
        let mut package_versions = BTreeMap::new();
        for package in &self.release.packages {
            if package_versions
                .insert(package.package_id(), package.package_version())
                .is_some()
            {
                return Err(CatalogIdentityError::DuplicateMember {
                    field: "release-packages",
                    id: package.package_id().to_owned(),
                });
            }
        }

        if let Some(entry) = &self.composed_entry {
            if let Some(path) = entry.reference.strip_prefix("file:") {
                if !std::path::Path::new(path).is_absolute() {
                    return invalid("a local composed entry must name an absolute file");
                }
            } else if crate::validate_composed_entry_path(&entry.reference).is_err() {
                return invalid(
                    "a composed entry requires the path entries@sha256:<manifest digest> \
                     relative to the component artifact base",
                );
            }
            ArtifactHash::parse(entry.route_digest.clone())?;
            if entry.members.is_empty() {
                return invalid("a composed entry must name its application members");
            }
            let mut identities = BTreeSet::new();
            for member in &entry.members {
                ArtifactHash::parse(member.digest.clone())?;
                if !identities.insert((&member.package_id, &member.component)) {
                    return invalid("a composed entry repeats a member identity");
                }
                if !self.components.iter().any(|component| {
                    component.package_id == member.package_id
                        && component.descriptor.component == member.component
                        && component.descriptor.component_digest == member.digest
                }) {
                    return invalid("a composed entry member is outside the release");
                }
            }
            if self
                .routes
                .iter()
                .any(|route| !identities.contains(&(&route.package_id, &route.component)))
            {
                return invalid("a composed entry omits a routed application member");
            }
        }

        let mut runtime_schemas = BTreeMap::new();
        for component in &self.components {
            if let Some(schema) = &component.runtime_schema
                && (schema.is_empty()
                    || schema.len() > 63
                    || !schema.bytes().enumerate().all(|(i, byte)| {
                        byte == b'_'
                            || byte.is_ascii_alphabetic()
                            || (i > 0 && byte.is_ascii_digit())
                    }))
            {
                return invalid("runtime schema must be a bare PostgreSQL identifier");
            }
            if let Some(previous) =
                runtime_schemas.insert(&component.package_id, &component.runtime_schema)
                && previous != &component.runtime_schema
            {
                return invalid("components of one application must carry the same runtime schema");
            }
            validate_package_member(&package_versions, &component.package_id)?;
            let descriptor = &component.descriptor;
            validate_text(&descriptor.component, "component")?;
            validate_text(&descriptor.interface_version, "interface-version")?;
            ArtifactHash::parse(descriptor.component_digest.clone())?;
            if component.operations.is_empty() {
                return invalid("a serving component must export at least one operation");
            }
            for (export, operation) in &component.operations {
                validate_text(export, "component-operation")?;
                validate_registered_operation(
                    &package_versions,
                    &component.package_id,
                    operation.registered_operation.as_deref(),
                )?;
                if let Some(registered) = &operation.registered_operation
                    && !operation.permissions.contains(registered)
                {
                    return invalid(format!(
                        "export {export:?} permissions omit its own registered operation"
                    ));
                }
                for permission in &operation.permissions {
                    validate_release_operation(&package_versions, permission)?;
                }
                if operation
                    .registered_operation
                    .as_deref()
                    .is_some_and(|registered| registered != export)
                {
                    return invalid(format!(
                        "component export {export:?} and registered operation {:?} differ",
                        operation.registered_operation
                    ));
                }
                if let Some(schema) = &operation.committed_result_schema {
                    let value: Value = serde_json::from_str(schema).map_err(|error| {
                        CatalogIdentityError::InvalidDefinition {
                            message: format!("committed-result schema is invalid JSON: {error}"),
                        }
                    })?;
                    if wamn_execution_contract::canonical_json_bytes(&value) != schema.as_bytes() {
                        return invalid("committed-result schema must be canonical JSON");
                    }
                    let normalized =
                        crate::component_facts::normalize_schema(value, "committed-result")
                            .and_then(|schema| {
                                crate::component_facts::validate_committed_result_schema(
                                    operation.registered_operation.as_deref(),
                                    Some(&schema),
                                )
                            });
                    normalized.map_err(|error| CatalogIdentityError::InvalidDefinition {
                        message: format!("serving component operation {export:?} carries invalid committed-result facts: {error}"),
                    })?;
                }
                crate::component_facts::validate_operation_statement_facts(
                    export,
                    &operation.statements,
                )
                .map_err(|error| CatalogIdentityError::InvalidDefinition {
                    message: format!(
                        "serving component operation {export:?} carries invalid statement facts: {error}"
                    ),
                })?;
            }
        }

        let mut routes = BTreeSet::new();
        for route in &self.routes {
            validate_package_member(&package_versions, &route.package_id)?;
            validate_text(&route.component, "component")?;
            validate_text(&route.operation, "operation")?;
            if route.type_ == OperationType::EventHandler {
                return invalid(format!(
                    "route {}::{} operation {:?} is an event handler, and an event handler is not a route",
                    route.package_id, route.component, route.operation
                ));
            }
            let providers = self
                .components
                .iter()
                .filter(|component| {
                    component.package_id == route.package_id
                        && component.descriptor.component == route.component
                        && component.operations.contains_key(&route.operation)
                })
                .count();
            if providers != 1 {
                return invalid(format!(
                    "route {}::{} operation {:?} resolves to {providers} release components",
                    route.package_id, route.component, route.operation
                ));
            }
            if !routes.insert((
                route.package_id.as_str(),
                route.component.as_str(),
                route.operation.as_str(),
            )) {
                return invalid(format!(
                    "route {}::{} operation {:?} occurs more than once",
                    route.package_id, route.component, route.operation
                ));
            }
        }

        for (attachment_id, attachment) in &self.attachments {
            validate_text(attachment_id, "attachment-id")?;
            validate_package_member(&package_versions, &attachment.package_id)?;
            validate_route_target(&routes, attachment_id, attachment)?;
            validate_attachment(
                &package_versions,
                attachment_id,
                &attachment.package_id,
                &attachment.definition,
                &attachment.auth_policy,
                attachment.registered_operation.as_deref(),
            )?;
        }

        let mut targets = BTreeSet::new();
        for wiring in &self.workflow.wirings {
            validate_package_member(&package_versions, &wiring.package_id)?;
            wiring.graph.validate()?;
            let (wiring_id, wiring_version, _) = wiring.identity();
            if !targets.insert((wiring.package_id.as_str(), wiring_id, wiring_version)) {
                return invalid("a wiring identity-version pair occurs more than once");
            }
        }

        for (attachment_id, attachment) in &self.workflow.attachments {
            validate_text(attachment_id, "attachment-id")?;
            if self.attachments.contains_key(attachment_id) {
                return invalid(format!(
                    "attachment {attachment_id:?} occurs both as a route attachment and in the workflow section"
                ));
            }
            validate_package_member(&package_versions, &attachment.package_id)?;
            validate_wiring_target(
                &targets,
                &attachment.package_id,
                &attachment.wiring_id,
                attachment.wiring_version,
            )?;
            validate_attachment(
                &package_versions,
                attachment_id,
                &attachment.package_id,
                &attachment.definition,
                &attachment.auth_policy,
                attachment.registered_operation.as_deref(),
            )?;
        }

        for (registration_id, registration) in &self.workflow.registrations {
            validate_package_member(&package_versions, &registration.package_id)?;
            validate_package_member(&package_versions, &registration.source_package_id)?;
            let (owner_package_id, local_registration_id) = registration_id
                .split_once("::")
                .ok_or_else(|| CatalogIdentityError::InvalidDefinition {
                    message: format!(
                        "registration key {registration_id:?} must be <package-id>::<registration-id>"
                    ),
                })?;
            validate_text(local_registration_id, "registration-id")?;
            if owner_package_id != registration.package_id || local_registration_id.contains("::") {
                return invalid(format!(
                    "registration key {registration_id:?} does not name owner package {:?}",
                    registration.package_id
                ));
            }
            validate_wiring_target(
                &targets,
                &registration.package_id,
                &registration.wiring_id,
                registration.wiring_version,
            )?;
            validate_text(&registration.entity, "entity")?;
            if registration.ops.is_empty() {
                return invalid("a registration matching no op is inert");
            }
            for op in &registration.ops {
                validate_text(op, "op")?;
            }
        }
        Ok(())
    }
}

fn validate_format_version(document: &Value) -> Result<(), CatalogIdentityError> {
    let Some(version) = document.get("format-version") else {
        return invalid("serving manifest format-version is required");
    };
    if version.as_u64() == Some(u64::from(SERVING_MANIFEST_FORMAT_VERSION)) {
        return Ok(());
    }
    let requested = match version {
        Value::String(value) => value.clone(),
        other => other.to_string(),
    };
    Err(CatalogIdentityError::UnsupportedServingManifestVersion { requested })
}

fn validate_route_target(
    routes: &BTreeSet<(&str, &str, &str)>,
    attachment_id: &str,
    attachment: &RouteAttachment,
) -> Result<(), CatalogIdentityError> {
    let component = attachment.component.as_str();
    let operation = attachment.operation.as_str();
    validate_text(component, "component")?;
    validate_text(operation, "operation")?;
    if !routes.contains(&(attachment.package_id.as_str(), component, operation)) {
        return Err(CatalogIdentityError::UnresolvableManifestRoute {
            package_id: attachment.package_id.clone(),
            component: component.to_owned(),
            operation: operation.to_owned(),
        });
    }
    if attachment
        .registered_operation
        .as_deref()
        .is_some_and(|registered| registered != operation)
    {
        return invalid(format!(
            "attachment {attachment_id:?} registered operation {:?} differs from its route operation {operation:?}",
            attachment.registered_operation
        ));
    }
    Ok(())
}

/// The checks that every attachment passes, whatever it targets.
fn validate_attachment(
    package_versions: &BTreeMap<&str, &str>,
    attachment_id: &str,
    package_id: &str,
    definition: &Value,
    auth_policy: &Value,
    registered_operation: Option<&str>,
) -> Result<(), CatalogIdentityError> {
    if !definition.is_object() {
        return invalid("attachment definition must be a JSON object");
    }
    let policy = parse_attachment_auth_policy(auth_policy).ok_or_else(|| {
        CatalogIdentityError::InvalidAttachmentAuthPolicy {
            attachment_id: attachment_id.to_owned(),
        }
    })?;
    if policy.has_no_caller() && registered_operation.is_some() {
        return Err(CatalogIdentityError::UnauthenticatedRegisteredOperation {
            attachment_id: attachment_id.to_owned(),
        });
    }
    if contains_retired_identity(definition) || contains_retired_identity(auth_policy) {
        return invalid("attachment configuration carries retired flow or plan identity");
    }
    validate_registered_operation(package_versions, package_id, registered_operation)
}

fn validate_wiring_target(
    targets: &BTreeSet<(&str, &str, u32)>,
    package_id: &str,
    wiring_id: &str,
    wiring_version: u32,
) -> Result<(), CatalogIdentityError> {
    validate_text(wiring_id, "wiring-id")?;
    if wiring_version == 0 {
        return Err(CatalogIdentityError::ZeroVersion {
            field: "wiring-version",
        });
    }
    if !targets.contains(&(package_id, wiring_id, wiring_version)) {
        return Err(CatalogIdentityError::UnresolvableManifestWiring {
            package_id: package_id.to_string(),
            wiring_id: wiring_id.to_string(),
            wiring_version,
        });
    }
    Ok(())
}

fn validate_package_member(
    package_versions: &BTreeMap<&str, &str>,
    package_id: &str,
) -> Result<(), CatalogIdentityError> {
    validate_text(package_id, "package-id")?;
    if !package_versions.contains_key(package_id) {
        return Err(CatalogIdentityError::InvalidDefinition {
            message: format!("package {package_id:?} is absent from the effective release"),
        });
    }
    Ok(())
}

fn validate_registered_operation(
    package_versions: &BTreeMap<&str, &str>,
    package_id: &str,
    operation: Option<&str>,
) -> Result<(), CatalogIdentityError> {
    let Some(operation) = operation else {
        return Ok(());
    };
    let package_version = package_versions
        .get(package_id)
        .expect("package membership was validated before operation identity");
    validate_canonical_operation_for_package(operation, package_id, package_version)
}

/// Require an operation identity that belongs to one release package.
fn validate_release_operation(
    package_versions: &BTreeMap<&str, &str>,
    operation: &str,
) -> Result<(), CatalogIdentityError> {
    if package_versions
        .iter()
        .any(|(package_id, package_version)| {
            validate_canonical_operation_for_package(operation, package_id, package_version).is_ok()
        })
    {
        return Ok(());
    }
    invalid(format!(
        "permission {operation:?} belongs to no package of the effective release"
    ))
}

fn contains_retired_identity(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "flow-id"
                    | "flow_id"
                    | "plan-hash"
                    | "plan_hash"
                    | "calls"
                    | "callable-contract"
                    | "source-artifact"
                    | "binding-base-artifact"
            ) || contains_retired_identity(value)
        }),
        Value::Array(values) => values.iter().any(contains_retired_identity),
        _ => false,
    }
}

fn invalid<T>(message: impl Into<String>) -> Result<T, CatalogIdentityError> {
    Err(CatalogIdentityError::InvalidDefinition {
        message: message.into(),
    })
}

fn within_delivery_limit(bytes: usize) -> Result<(), CatalogIdentityError> {
    if bytes > MAX_SERVING_MANIFEST_BYTES {
        return Err(CatalogIdentityError::ManifestTooLarge {
            bytes,
            limit: MAX_SERVING_MANIFEST_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture_graph;

    const COMPONENT_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const COMPONENT_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const DEFINITION: &str =
        "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    fn definition_hash(value: &str) -> DefinitionHash {
        DefinitionHash::parse(value).expect("fixture definition hash is canonical")
    }

    fn release() -> ServingRelease {
        ServingRelease {
            packages: BTreeSet::from([
                PackageCoordinate::new("base", "1.0.0").unwrap(),
                PackageCoordinate::new("overlay", "3.0.0").unwrap(),
            ]),
        }
    }

    fn components() -> BTreeSet<ServingComponent> {
        BTreeSet::from([
            ServingComponent {
                runtime_schema: None,
                statement_instances: std::collections::BTreeMap::new(),
                package_id: "overlay".into(),
                operations: BTreeMap::from([(
                    "overlay:transform/map@3.0.0".into(),
                    ServingComponentOperation {
                        committed_result_schema: None,
                        registered_operation: Some("overlay:transform/map@3.0.0".into()),
                        permissions: BTreeSet::from([
                            "base:widget/get@1.0.0".into(),
                            "overlay:transform/map@3.0.0".into(),
                        ]),
                        statements: BTreeMap::new(),
                    },
                )]),
                descriptor: ComponentDescriptor::named("transform", "0.1", COMPONENT_B),
            },
            ServingComponent {
                runtime_schema: None,
                statement_instances: std::collections::BTreeMap::new(),
                package_id: "base".into(),
                operations: BTreeMap::from([(
                    "base:widget/get@1.0.0".into(),
                    ServingComponentOperation {
                        committed_result_schema: None,
                        registered_operation: Some("base:widget/get@1.0.0".into()),
                        permissions: BTreeSet::from(["base:widget/get@1.0.0".into()]),
                        statements: BTreeMap::new(),
                    },
                )]),
                descriptor: ComponentDescriptor::named("http-request", "0.1", COMPONENT_A),
            },
        ])
    }

    fn wirings() -> BTreeSet<ServingWiring> {
        BTreeSet::from([
            ServingWiring::new("overlay", fixture_graph("shipping", 2)),
            ServingWiring::new("base", fixture_graph("orders", 3)),
        ])
    }

    fn routes() -> BTreeSet<ServingRoute> {
        BTreeSet::from([ServingRoute {
            package_id: "base".into(),
            component: "http-request".into(),
            operation: "base:widget/get@1.0.0".into(),
            type_: OperationType::Get,
            reads: BTreeSet::new(),
            revision: None,
            idempotency: None,
            canonicalization: None,
        }])
    }

    fn route_attachment() -> ServingAttachment {
        ServingAttachment {
            target: AttachmentTarget::Route {
                component: "http-request".into(),
                operation: "base:widget/get@1.0.0".into(),
            },
            ..attachment()
        }
    }

    fn attachment() -> ServingAttachment {
        ServingAttachment {
            type_: AttachmentType::Http,
            package_id: "base".into(),
            target: AttachmentTarget::Wiring {
                wiring_id: "orders".into(),
                wiring_version: 3,
            },
            definition_hash: definition_hash(DEFINITION),
            definition: serde_json::json!({
                "id": "orders",
                "kind": "http",
                "route": {"host": "*", "path": "/orders", "method": "POST"}
            }),
            auth_policy: serde_json::json!({"modes": ["pat"]}),
            registered_operation: Some("base:widget/get@1.0.0".into()),
        }
    }

    fn registration() -> ServingRegistration {
        ServingRegistration {
            package_id: "overlay".into(),
            source_package_id: "base".into(),
            wiring_id: "shipping".into(),
            wiring_version: 2,
            entity: "orders".into(),
            ops: BTreeSet::from(["insert".to_string()]),
            input: ServingRegistrationInput::Event,
            delivery: RegistrationDelivery::Queue,
        }
    }

    fn manifest() -> ServingManifest {
        ServingManifest::new(
            release(),
            components(),
            routes(),
            wirings(),
            BTreeMap::from([
                ("orders".to_string(), attachment()),
                ("widget-get".to_string(), route_attachment()),
            ]),
            BTreeMap::from([("overlay::orders-changed".to_string(), registration())]),
        )
        .expect("fixture manifest is valid")
    }

    fn entry_for(manifest: &ServingManifest) -> ServingComposedEntry {
        ServingComposedEntry {
            reference: format!("entries@sha256:{}", "e".repeat(64)),
            route_digest: format!("sha256:{}", "f".repeat(64)),
            members: manifest
                .components
                .iter()
                .map(|component| ServingEntryMember {
                    package_id: component.package_id.clone(),
                    component: component.descriptor.component.clone(),
                    digest: component.descriptor.component_digest.clone(),
                })
                .collect(),
        }
    }

    #[test]
    fn composed_entry_identity_is_canonical_and_changes_release_digest() {
        let mut manifest = manifest();
        let original = manifest.digest();
        manifest.composed_entry = Some(entry_for(&manifest));
        let bytes = manifest.canonical_bytes();
        let (decoded, digest) = ServingManifest::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded, manifest);
        assert_ne!(digest, original);
        manifest.composed_entry.as_mut().unwrap().reference =
            format!("entries@sha256:{}", "b".repeat(64));
        let reference_digest = manifest.digest();
        assert_ne!(reference_digest, digest);
        manifest.composed_entry.as_mut().unwrap().route_digest =
            format!("sha256:{}", "a".repeat(64));
        assert_ne!(manifest.digest(), reference_digest);
    }

    fn component_of(manifest: &ServingManifest, package_id: &str) -> ServingComponent {
        manifest
            .components
            .iter()
            .find(|component| component.package_id == package_id)
            .expect("the fixture names the package")
            .clone()
    }

    #[test]
    fn a_base_operation_reached_through_the_composed_entry_runs_as_the_composing_application() {
        let mut manifest = manifest();
        // Without an entry, each component is its own application.
        assert_eq!(
            manifest.composing_application(&component_of(&manifest, "base")),
            "base"
        );
        // The overlay composes base: it binds base's statements through a
        // named import.
        let mut overlay = component_of(&manifest, "overlay");
        overlay.statement_instances = BTreeMap::from([("sql-0".to_owned(), BTreeMap::new())]);
        manifest.components = manifest
            .components
            .into_iter()
            .filter(|component| component.package_id != "overlay")
            .chain([overlay])
            .collect();
        manifest.composed_entry = Some(entry_for(&manifest));
        assert_eq!(
            manifest
                .composing_member()
                .map(|component| component.package_id.as_str()),
            Some("overlay")
        );
        for package_id in ["base", "overlay"] {
            assert_eq!(
                manifest.composing_application(&component_of(&manifest, package_id)),
                "overlay",
                "{package_id} runs as the composing application"
            );
        }
        // A component outside the entry stays its own application.
        manifest
            .composed_entry
            .as_mut()
            .unwrap()
            .members
            .retain(|member| member.package_id == "base");
        assert_eq!(
            manifest.composing_application(&component_of(&manifest, "overlay")),
            "overlay"
        );
        assert_eq!(
            manifest.composing_application(&component_of(&manifest, "base")),
            "base"
        );
    }

    #[test]
    fn local_composed_entry_requires_an_absolute_file_reference() {
        let mut manifest = manifest();
        let mut entry = entry_for(&manifest);
        entry.reference = "file:/var/tmp/application/entry.wasm".into();
        manifest.composed_entry = Some(entry);
        assert!(ServingManifest::from_canonical_bytes(&manifest.canonical_bytes()).is_ok());
        manifest.composed_entry.as_mut().unwrap().reference = "file:entry.wasm".into();
        assert!(ServingManifest::from_canonical_bytes(&manifest.canonical_bytes()).is_err());
    }

    #[test]
    fn composed_entry_refuses_an_absolute_reference() {
        let mut manifest = manifest();
        let mut entry = entry_for(&manifest);
        entry.reference = format!(
            "172.28.0.6:5000/wamn/releases/entries@sha256:{}",
            "e".repeat(64)
        );
        manifest.composed_entry = Some(entry);
        assert!(ServingManifest::from_canonical_bytes(&manifest.canonical_bytes()).is_err());
    }

    #[test]
    fn composed_entry_refuses_invalid_or_unreleased_members() {
        let manifest = manifest();
        let entry = entry_for(&manifest);
        for change in 0..5 {
            let mut manifest = manifest.clone();
            let mut entry = entry.clone();
            match change {
                0 => entry.reference = "registry.example/entry:latest".into(),
                1 => entry.route_digest = "invalid".into(),
                2 => entry.members.clear(),
                3 => {
                    let mut member = entry.members.pop_first().unwrap();
                    member.digest = format!("sha256:{}", "0".repeat(64));
                    entry.members.insert(member);
                }
                _ => {
                    let mut member = entry.members.first().unwrap().clone();
                    member.component = "outside-release".into();
                    entry.members.insert(member);
                }
            }
            manifest.composed_entry = Some(entry);
            assert!(ServingManifest::from_canonical_bytes(&manifest.canonical_bytes()).is_err());
        }
    }

    #[test]
    fn format_ten_requires_republication() {
        let mut manifest = manifest();
        manifest.format_version = 10;
        assert!(matches!(
            ServingManifest::from_canonical_bytes(&manifest.canonical_bytes()),
            Err(CatalogIdentityError::UnsupportedServingManifestVersion { .. })
        ));
    }

    #[test]
    fn runtime_schema_is_pinned_and_consistent_within_an_application() {
        let original = manifest();
        let mut pinned = original.clone();
        pinned.components = pinned
            .components
            .into_iter()
            .map(|mut component| {
                component.runtime_schema = Some(format!("schema_{}", component.package_id));
                component
            })
            .collect();
        let (loaded, digest) =
            ServingManifest::from_canonical_bytes(&pinned.canonical_bytes()).unwrap();
        assert_eq!(loaded, pinned);
        assert_ne!(digest, original.digest());
        let mut conflicting = pinned.components.first().unwrap().clone();
        conflicting.descriptor.component = "another".into();
        conflicting.runtime_schema = Some("other_schema".into());
        pinned.components.insert(conflicting);
        assert!(ServingManifest::from_canonical_bytes(&pinned.canonical_bytes()).is_err());
        let mut invalid = original;
        invalid.components = invalid
            .components
            .into_iter()
            .map(|mut component| {
                component.runtime_schema = Some("first, second".into());
                component
            })
            .collect();
        assert!(ServingManifest::from_canonical_bytes(&invalid.canonical_bytes()).is_err());
    }

    #[test]
    fn stored_schedules_are_read_with_the_field_ignored() {
        let manifest = manifest();
        let mut document = serde_json::to_value(&manifest).unwrap();
        document["workflow"]["schedules"] = serde_json::json!({
            "base::hourly": {
                "package-id": "base",
                "workflow-id": "hourly",
                "wiring-id": "orders",
                "wiring-version": 3,
                "schedule": "0 * * * *",
            },
        });
        let bytes = wamn_execution_contract::canonical_json_bytes(&document);
        let (decoded, digest) = ServingManifest::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(decoded, manifest);
        assert_eq!(
            digest.as_str(),
            wamn_execution_contract::canonical_json_sha256(&document)
        );
        assert_ne!(digest, manifest.digest());
        let mut spaced = bytes.clone();
        spaced.push(b' ');
        assert!(ServingManifest::from_canonical_bytes(&spaced).is_err());
    }

    #[test]
    fn committed_result_schema_is_canonical_and_changes_the_release_identity() {
        let original = manifest();
        let mut document = serde_json::to_value(&original).unwrap();
        let operation = document["components"][0]["operations"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        document["components"][0]["operations"][&operation]["committed-result-schema"] =
            serde_json::json!("{\"type\":\"array\"}");
        let (declared, digest) = ServingManifest::from_canonical_bytes(
            &wamn_execution_contract::canonical_json_bytes(&document),
        )
        .expect("canonical committed schema loads");
        assert_ne!(digest, original.digest());
        assert_eq!(serde_json::to_value(&declared).unwrap(), document);
        for schema in ["{ \"type\": \"array\" }", "{\"type\":\"unknown\"}", "{"] {
            document["components"][0]["operations"][&operation]["committed-result-schema"] =
                serde_json::json!(schema);
            assert!(
                ServingManifest::from_canonical_bytes(
                    &wamn_execution_contract::canonical_json_bytes(&document),
                )
                .is_err(),
                "{schema}"
            );
        }
        document["components"][0]["operations"][&operation]["committed-result-schema"] =
            serde_json::json!("{}");
        document["components"][0]["operations"][&operation]
            .as_object_mut()
            .unwrap()
            .remove("registered-operation");
        assert!(
            ServingManifest::from_canonical_bytes(&wamn_execution_contract::canonical_json_bytes(
                &document
            ),)
            .is_err()
        );
    }

    #[test]
    fn collection_insertion_order_cannot_reach_identity() {
        let forward = manifest();
        let reversed = ServingManifest::new(
            release(),
            components().into_iter().rev().collect(),
            routes().into_iter().rev().collect(),
            wirings().into_iter().rev().collect(),
            BTreeMap::from([
                ("widget-get".to_string(), route_attachment()),
                ("orders".to_string(), attachment()),
            ]),
            BTreeMap::from([("overlay::orders-changed".to_string(), registration())]),
        )
        .expect("reordered fixture is valid");

        assert_eq!(forward.canonical_bytes(), reversed.canonical_bytes());
        assert_eq!(forward.digest(), reversed.digest());
    }

    fn admitted(
        package_id: &str,
        package_version: &str,
        digest: &str,
        operations: Vec<(&str, crate::AdmittedComponentOperation)>,
    ) -> AdmittedComponent {
        AdmittedComponent {
            scope: crate::ComponentPackageScope {
                tenant_id: "t1".into(),
                package_id: package_id.into(),
                package_version: package_version.into(),
            },
            component: package_id.into(),
            interface_version: "0.1".into(),
            operations: operations
                .into_iter()
                .map(|(name, operation)| (name.to_owned(), operation))
                .collect(),
            component_digest: digest.into(),
            imports: Vec::new(),
            imports_fingerprint: digest.into(),
            effects: Vec::new(),
        }
    }

    /// One overlay operation calls two members. Each member's import binds
    /// only that member's own statements, and the overlay's binds its own.
    #[test]
    fn each_member_of_one_operation_binds_only_its_own_statements() {
        const COMPONENT_C: &str =
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        const OVERLAY_MEMBER: &str =
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
        let first = admitted(
            "first",
            "1.0.0",
            COMPONENT_A,
            vec![(
                "first:op/run@1.0.0",
                admitted_operation("first:op/run@1.0.0", Some("SELECT 'first'")),
            )],
        );
        let second = admitted(
            "second",
            "1.0.0",
            COMPONENT_C,
            vec![(
                "second:op/run@1.0.0",
                admitted_operation("second:op/run@1.0.0", Some("SELECT 'second'")),
            )],
        );
        let overlay = admitted(
            "overlay",
            "3.0.0",
            COMPONENT_B,
            vec![(
                "overlay:op/run@3.0.0",
                admitted_operation("overlay:op/run@3.0.0", Some("SELECT 'overlay'")),
            )],
        );
        let bases = [&first, &second];
        let base = |digest: &str| {
            bases
                .iter()
                .copied()
                .find(|base| base.component_digest == digest)
        };
        let instances = BTreeMap::from([
            ("sql-0".to_owned(), COMPONENT_A.to_owned()),
            ("sql-1".to_owned(), COMPONENT_C.to_owned()),
            ("sql-2".to_owned(), OVERLAY_MEMBER.to_owned()),
        ]);
        let served = ServingComponent::project(&overlay, Vec::new())
            .expect("the overlay projects")
            .with_statement_instances(&instances, &base)
            .expect("each member binds its own statements");

        let sql = |instance: &str| {
            served.statement_instances[instance]
                .values()
                .map(|statement| statement.sql.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(sql("sql-0"), ["SELECT 'first'"]);
        assert_eq!(sql("sql-1"), ["SELECT 'second'"]);
        assert_eq!(sql("sql-2"), ["SELECT 'overlay'"]);
        assert_eq!(served.descriptor.statement_instances, instances);

        let mut unmatched = instances;
        unmatched.insert("sql-3".to_owned(), COMPONENT_B.to_owned());
        ServingComponent::project(&overlay, Vec::new())
            .expect("the overlay projects")
            .with_statement_instances(&unmatched, &base)
            .expect_err("two members that match no base are refused");
    }

    fn admitted_operation(
        registered: &str,
        statement: Option<&str>,
    ) -> crate::AdmittedComponentOperation {
        crate::AdmittedComponentOperation {
            registered_operation: Some(registered.into()),
            committed_result_schema: None,
            host_transaction: false,
            input_ports: Vec::new(),
            output_ports: Vec::new(),
            parameters: Vec::new(),
            statements: statement
                .map(|sql| {
                    let digest = crate::digest(sql.as_bytes());
                    BTreeMap::from([(
                        digest,
                        ComponentSqlStatement {
                            name: "statement".into(),
                            path: "statement.sql".into(),
                            sql: sql.into(),
                            binds: Vec::new(),
                            columns: Vec::new(),
                            transactional: true,
                        },
                    )])
                })
                .unwrap_or_default(),
        }
    }

    #[test]
    fn projection_carries_each_exports_own_statements_only() {
        const OVERLAY: &str = "overlay:receiving/record@3.0.0";
        let overlay = admitted(
            "overlay",
            "3.0.0",
            COMPONENT_B,
            vec![(
                OVERLAY,
                admitted_operation(OVERLAY, Some("INSERT INTO inspection DEFAULT VALUES")),
            )],
        );
        let projected =
            ServingComponent::project(&overlay, Vec::new()).expect("the overlay projects");
        let entry = &projected.operations[OVERLAY];
        assert_eq!(
            entry.permissions,
            BTreeSet::from([OVERLAY.into()]),
            "the entry requires its own permission only"
        );
        assert_eq!(
            entry.statements, overlay.operations[OVERLAY].statements,
            "the entry holds its own SQL; a base's SQL is the base member's"
        );
    }

    #[test]
    fn permissions_hold_the_registered_operation_and_release_packages_only() {
        let mut missing_own = components();
        let mut overlay = missing_own
            .iter()
            .find(|component| component.package_id == "overlay")
            .cloned()
            .unwrap();
        missing_own.remove(&overlay);
        overlay
            .operations
            .get_mut("overlay:transform/map@3.0.0")
            .unwrap()
            .permissions
            .remove("overlay:transform/map@3.0.0");
        missing_own.insert(overlay.clone());
        let error = ServingManifest::new(
            release(),
            missing_own,
            routes(),
            wirings(),
            BTreeMap::from([("orders".to_string(), attachment())]),
            BTreeMap::from([("overlay::orders-changed".to_string(), registration())]),
        )
        .expect_err("permissions without the export's own operation were accepted");
        assert!(
            error
                .to_string()
                .contains("omit its own registered operation")
        );

        let mut outside = components();
        outside.remove(&overlay);
        let operation = overlay
            .operations
            .get_mut("overlay:transform/map@3.0.0")
            .unwrap();
        operation
            .permissions
            .insert("overlay:transform/map@3.0.0".into());
        operation
            .permissions
            .insert("stranger:widget/get@1.0.0".into());
        outside.insert(overlay);
        let error = ServingManifest::new(
            release(),
            outside,
            routes(),
            wirings(),
            BTreeMap::from([("orders".to_string(), attachment())]),
            BTreeMap::from([("overlay::orders-changed".to_string(), registration())]),
        )
        .expect_err("a permission outside the release was accepted");
        assert!(error.to_string().contains("belongs to no package"));
    }

    /// A wiring is its package and its graph. The graph carries the id,
    /// version and hash, so a wiring that restates them is refused.
    #[test]
    fn a_wiring_is_its_package_and_graph() {
        let manifest = manifest();
        let wiring = manifest.workflow.wirings.first().expect("a fixture wiring");
        let (wiring_id, wiring_version, graph_hash) = wiring.identity();
        assert_eq!(wiring_id, wiring.graph.wiring_id);
        assert_eq!(wiring_version, wiring.graph.version);
        assert_eq!(graph_hash, wiring.graph.wiring_hash());

        for (field, value) in [
            ("wiring-id", serde_json::json!(wiring_id)),
            ("wiring-version", serde_json::json!(wiring_version)),
            ("graph-hash", serde_json::json!(graph_hash.as_str())),
        ] {
            let mut document: Value = serde_json::from_slice(&manifest.canonical_bytes()).unwrap();
            document["workflow"]["wirings"][0][field] = value;
            assert!(
                ServingManifest::from_canonical_bytes(
                    &wamn_execution_contract::canonical_json_bytes(&document)
                )
                .is_err(),
                "a wiring that restates {field} was admitted"
            );
        }
    }

    #[test]
    fn only_canonical_format_six_bytes_are_admitted() {
        let manifest = manifest();
        let bytes = manifest.canonical_bytes();
        assert_eq!(
            ServingManifest::from_canonical_bytes(&bytes),
            Ok((manifest.clone(), manifest.digest()))
        );

        let value: Value = serde_json::from_slice(&bytes).expect("canonical bytes are JSON");
        let indented = serde_json::to_vec_pretty(&value).expect("document serializes");
        assert_eq!(
            ServingManifest::from_canonical_bytes(&indented),
            Err(CatalogIdentityError::NonCanonicalJson)
        );
    }

    /// A component is its package, its descriptor and its operations, and
    /// nothing else: the descriptor is the one owner of the component name,
    /// interface version and digest. The canonical bytes carry it whole.
    #[test]
    fn a_component_holds_its_package_descriptor_and_operations_only() {
        let manifest = manifest();
        let value: Value = serde_json::from_slice(&manifest.canonical_bytes()).unwrap();
        for (index, component) in manifest.components.iter().enumerate() {
            let keys = value["components"][index]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>();
            assert_eq!(keys, ["descriptor", "operations", "package-id"]);
            assert_eq!(
                value["components"][index]["descriptor"],
                serde_json::to_value(&component.descriptor).unwrap()
            );
        }
    }

    #[test]
    fn statement_lookup_is_operation_scoped_and_revalidates_its_key() {
        let operation = "base:widget/get@1.0.0";
        let sql = "SELECT row_version FROM widget WHERE id = $1";
        let path = "generated/sql/widget/get.sql".to_owned();
        let mut components = components();
        let mut base = components
            .iter()
            .find(|component| component.package_id == "base")
            .expect("fixture has a base component")
            .clone();
        components.remove(&base);
        base.operations
            .get_mut(operation)
            .expect("fixture has the base operation")
            .statements
            .insert(
                path.clone(),
                ComponentSqlStatement {
                    name: "get".into(),
                    path: path.clone(),
                    sql: sql.into(),
                    // A SELECT fixture: PostgreSQL classifies it as needing no
                    // transaction.
                    transactional: false,
                    binds: Vec::new(),
                    columns: Vec::new(),
                },
            );
        components.insert(base.clone());

        let manifest = ServingManifest::new(
            release(),
            components.clone(),
            routes(),
            wirings(),
            BTreeMap::from([("orders".to_string(), attachment())]),
            BTreeMap::from([("overlay::orders-changed".to_string(), registration())]),
        )
        .expect("an exact operation-scoped statement enters the serving manifest");
        let admitted = manifest
            .components
            .iter()
            .find(|component| component.package_id == "base")
            .expect("base component remains present")
            .operations[operation]
            .statement(&path);
        assert!(admitted.is_some());
        assert!(
            manifest
                .components
                .iter()
                .find(|component| component.package_id == "overlay")
                .expect("overlay component remains present")
                .operations["overlay:transform/map@3.0.0"]
                .statement(&path)
                .is_none()
        );

        let mut broken = components;
        let mut base = broken
            .iter()
            .find(|component| component.package_id == "base")
            .expect("fixture has a base component")
            .clone();
        broken.remove(&base);
        base.operations
            .get_mut(operation)
            .expect("fixture has the base operation")
            .statements
            .get_mut(&path)
            .expect("fixture has a statement")
            .path
            .push_str(".moved");
        broken.insert(base);
        let error = ServingManifest::new(
            release(),
            broken,
            routes(),
            wirings(),
            BTreeMap::from([("orders".to_string(), attachment())]),
            BTreeMap::from([("overlay::orders-changed".to_string(), registration())]),
        )
        .expect_err("a statement may not move off its key");
        assert!(error.to_string().contains("invalid statement facts"));
    }

    #[test]
    fn unsupported_formats_are_typed_refusals_not_compatibility_arms() {
        for version in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12] {
            let unsupported = serde_json::to_vec(&serde_json::json!({
                "format-version": version,
                "release": {}
            }))
            .unwrap();
            let error = ServingManifest::from_canonical_bytes(&unsupported)
                .expect_err("only format eleven enters the decoder");
            assert_eq!(
                error,
                CatalogIdentityError::UnsupportedServingManifestVersion {
                    requested: version.to_string()
                }
            );
            assert!(
                error
                    .to_string()
                    .starts_with(UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL)
            );
        }
    }

    #[test]
    fn attachment_operation_is_explicit_and_canonical() {
        let mut malformed = attachment();
        malformed.registered_operation = Some("widget.get".into());
        let error = ServingManifest::new(
            release(),
            components(),
            routes(),
            wirings(),
            BTreeMap::from([("orders".to_string(), malformed)]),
            BTreeMap::new(),
        )
        .expect_err("an attachment cannot smuggle a local-only operation token");
        assert!(
            error
                .to_string()
                .contains("<package-id>:<module>/<action>@<package-version>")
        );
    }

    #[test]
    fn registered_operations_match_the_containing_package_coordinate() {
        for operation in ["overlay:widget/get@3.0.0", "base:widget/get@2.0.0"] {
            let mut mismatched = attachment();
            mismatched.registered_operation = Some(operation.into());
            let error = ServingManifest::new(
                release(),
                components(),
                routes(),
                wirings(),
                BTreeMap::from([("orders".to_string(), mismatched)]),
                BTreeMap::new(),
            )
            .expect_err("attachment operation must match its selected package coordinate");
            assert!(error.to_string().contains("does not belong"));
        }

        let mut mismatched_components = components();
        let mut component = mismatched_components
            .pop_first()
            .expect("fixture has a component");
        let component_operation = component
            .operations
            .values_mut()
            .next()
            .expect("fixture component has an operation");
        component_operation.registered_operation = Some("overlay:widget/get@3.0.0".into());
        mismatched_components.insert(component);
        let error = ServingManifest::new(
            release(),
            mismatched_components,
            routes(),
            wirings(),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .expect_err("component operation must match its selected package coordinate");
        assert!(error.to_string().contains("does not belong"));
    }

    #[test]
    fn signature_policy_has_exact_fields_and_no_caller() {
        let policy = serde_json::json!({
            "modes": ["signature"], "header": "X-Signature", "credential": "webhook"
        });
        let parsed = parse_attachment_auth_policy(&policy).unwrap();
        assert_eq!(
            parsed,
            AttachmentAuthPolicy::Signature {
                header: "X-Signature",
                credential: "webhook"
            }
        );
        assert!(parsed.has_no_caller());
        assert!(!parsed.allows_pat());
        assert!(!parsed.allows_session());
        for malformed in [
            serde_json::json!({"modes":["signature"]}),
            serde_json::json!({"modes":["signature"],"header":"","credential":"webhook"}),
            serde_json::json!({"modes":["signature"],"header":"X-Signature","credential":""}),
            serde_json::json!({"modes":["signature"],"header":null,"credential":"webhook"}),
            serde_json::json!({"modes":["signature"],"header":"X-Signature","credential":1}),
            serde_json::json!({"modes":["signature"],"header":"X-Signature","credential":"webhook","extra":true}),
            serde_json::json!({"modes":["signature","pat"],"header":"X-Signature","credential":"webhook"}),
            serde_json::json!({"modes":["session","signature"],"header":"X-Signature","credential":"webhook"}),
            serde_json::json!({"modes":["none","signature"],"header":"X-Signature","credential":"webhook"}),
            serde_json::json!({"modes":["signature","signature"],"header":"X-Signature","credential":"webhook"}),
        ] {
            assert_eq!(
                parse_attachment_auth_policy(&malformed),
                None,
                "{malformed}"
            );
        }
        let mut signed = attachment();
        signed.auth_policy = policy;
        let build = |signed| {
            ServingManifest::new(
                release(),
                components(),
                routes(),
                wirings(),
                BTreeMap::from([("orders".into(), signed)]),
                BTreeMap::new(),
            )
        };
        assert_eq!(
            build(signed.clone()),
            Err(CatalogIdentityError::UnauthenticatedRegisteredOperation {
                attachment_id: "orders".into(),
            })
        );
        signed.registered_operation = None;
        let manifest = build(signed).unwrap();
        assert_eq!(manifest.format_version, 11);
        let (decoded, _) =
            ServingManifest::from_canonical_bytes(&manifest.canonical_bytes()).unwrap();
        assert_eq!(decoded, manifest);
        let mut old = manifest;
        old.format_version = 10;
        assert_eq!(
            ServingManifest::from_canonical_bytes(&old.canonical_bytes()).unwrap_err(),
            CatalogIdentityError::UnsupportedServingManifestVersion {
                requested: "10".into()
            }
        );
    }

    #[test]
    fn attachment_auth_policy_modes_are_closed_and_required() {
        for policy in [
            serde_json::json!(null),
            serde_json::json!({}),
            serde_json::json!({"mode": 7}),
            serde_json::json!({"mode": "invented"}),
            serde_json::json!({"mode": "pat"}),
            serde_json::json!({"modes": []}),
            serde_json::json!({"modes": "pat"}),
            serde_json::json!({"modes": ["pat", "pat"]}),
            serde_json::json!({"modes": ["none", "pat"]}),
            serde_json::json!({"modes": ["session", "pat"]}),
            serde_json::json!({"modes": ["invented"]}),
            serde_json::json!({"modes": ["pat"], "mode": "pat"}),
        ] {
            let mut malformed = attachment();
            malformed.auth_policy = policy;
            assert_eq!(
                ServingManifest::new(
                    release(),
                    components(),
                    routes(),
                    wirings(),
                    BTreeMap::from([("orders".to_string(), malformed)]),
                    BTreeMap::new(),
                ),
                Err(CatalogIdentityError::InvalidAttachmentAuthPolicy {
                    attachment_id: "orders".into(),
                })
            );
        }

        for modes in [
            serde_json::json!([PAT_AUTHENTICATION_MODE]),
            serde_json::json!([SESSION_AUTHENTICATION_MODE]),
            serde_json::json!([PAT_AUTHENTICATION_MODE, SESSION_AUTHENTICATION_MODE]),
        ] {
            let mut authenticated = attachment();
            authenticated.auth_policy = serde_json::json!({"modes": modes});
            ServingManifest::new(
                release(),
                components(),
                routes(),
                wirings(),
                BTreeMap::from([("orders".to_string(), authenticated)]),
                BTreeMap::new(),
            )
            .expect("authenticated modes admit a registered operation");
        }

        let mut anonymous = attachment();
        anonymous.auth_policy = serde_json::json!({"modes": [NO_AUTHENTICATION_MODE]});
        assert_eq!(
            ServingManifest::new(
                release(),
                components(),
                routes(),
                wirings(),
                BTreeMap::from([("orders".to_string(), anonymous)]),
                BTreeMap::new(),
            ),
            Err(CatalogIdentityError::UnauthenticatedRegisteredOperation {
                attachment_id: "orders".into(),
            })
        );
    }

    #[test]
    fn one_effective_release_selects_only_one_version_of_each_package() {
        let mut duplicate = release();
        duplicate
            .packages
            .insert(PackageCoordinate::new("base", "2.0.0").unwrap());
        let error = ServingManifest::new(
            duplicate,
            components(),
            routes(),
            wirings(),
            BTreeMap::new(),
            BTreeMap::new(),
        )
        .expect_err("two versions of one package must refuse");
        assert_eq!(
            error,
            CatalogIdentityError::DuplicateMember {
                field: "release-packages",
                id: "base".into(),
            }
        );
    }

    #[test]
    fn attachment_and_registration_targets_are_exact() {
        let mut wrong_version = attachment();
        wrong_version.target = AttachmentTarget::Wiring {
            wiring_id: "orders".into(),
            wiring_version: 2,
        };
        assert_eq!(
            ServingManifest::new(
                release(),
                components(),
                routes(),
                wirings(),
                BTreeMap::from([("orders".to_string(), wrong_version)]),
                BTreeMap::new(),
            ),
            Err(CatalogIdentityError::UnresolvableManifestWiring {
                package_id: "base".into(),
                wiring_id: "orders".into(),
                wiring_version: 2,
            })
        );

        let mut missing = registration();
        missing.wiring_id = "ghost".into();
        assert_eq!(
            ServingManifest::new(
                release(),
                components(),
                routes(),
                wirings(),
                BTreeMap::new(),
                BTreeMap::from([("overlay::orders-changed".to_string(), missing)]),
            ),
            Err(CatalogIdentityError::UnresolvableManifestWiring {
                package_id: "overlay".into(),
                wiring_id: "ghost".into(),
                wiring_version: 2,
            })
        );

        let mut wrong_package = attachment();
        wrong_package.package_id = "overlay".into();
        assert_eq!(
            ServingManifest::new(
                release(),
                components(),
                routes(),
                wirings(),
                BTreeMap::from([("orders".to_string(), wrong_package)]),
                BTreeMap::new(),
            ),
            Err(CatalogIdentityError::UnresolvableManifestWiring {
                package_id: "overlay".into(),
                wiring_id: "orders".into(),
                wiring_version: 3,
            })
        );
    }

    #[test]
    fn registration_identity_keeps_owner_and_emitter_distinct() {
        let mut registrations = BTreeMap::from([
            (
                "base::widget-created".to_owned(),
                ServingRegistration {
                    package_id: "base".into(),
                    source_package_id: "base".into(),
                    wiring_id: "orders".into(),
                    wiring_version: 3,
                    entity: "widget".into(),
                    ops: BTreeSet::from(["insert".into()]),
                    input: ServingRegistrationInput::Event,
                    delivery: RegistrationDelivery::Queue,
                },
            ),
            ("overlay::widget-created".to_owned(), registration()),
        ]);
        ServingManifest::new(
            release(),
            components(),
            routes(),
            wirings(),
            BTreeMap::new(),
            registrations.clone(),
        )
        .expect("the same local registration id remains distinct by owner package");

        let overlay = registrations
            .get_mut("overlay::widget-created")
            .expect("fixture carries the overlay registration");
        overlay.source_package_id = "missing".into();
        let error = ServingManifest::new(
            release(),
            components(),
            routes(),
            wirings(),
            BTreeMap::new(),
            registrations,
        )
        .expect_err("the emitter must be an exact member of the release");
        assert!(
            error
                .to_string()
                .contains("absent from the effective release")
        );
    }

    #[test]
    fn registration_map_key_is_owner_qualified() {
        for key in ["orders-changed", "base::orders-changed", "overlay::bad::id"] {
            let error = ServingManifest::new(
                release(),
                components(),
                routes(),
                wirings(),
                BTreeMap::new(),
                BTreeMap::from([(key.to_owned(), registration())]),
            )
            .expect_err("a registration key must carry its exact owner coordinate");
            assert!(error.to_string().contains("registration key"));
        }
    }

    #[test]
    fn removed_flow_and_plan_fields_are_refused() {
        let mut document = serde_json::to_value(manifest()).expect("manifest serializes");
        document["flows"] = serde_json::json!({});
        assert!(serde_json::from_value::<ServingManifest>(document).is_err());
    }

    #[test]
    fn the_delivery_ceiling_is_enforced_at_the_reader() {
        let mount = vec![b'x'; MAX_SERVING_MANIFEST_BYTES + 1];
        assert_eq!(
            ServingManifest::from_canonical_bytes(&mount),
            Err(CatalogIdentityError::ManifestTooLarge {
                bytes: MAX_SERVING_MANIFEST_BYTES + 1,
                limit: MAX_SERVING_MANIFEST_BYTES,
            })
        );
    }

    #[test]
    fn an_attachment_target_is_flat_on_the_wire_and_names_exactly_one_target() {
        let route = serde_json::to_value(route_attachment()).expect("attachment serializes");
        assert_eq!(route["component"], "http-request");
        assert_eq!(route["operation"], "base:widget/get@1.0.0");
        assert!(route.get("wiring-id").is_none() && route.get("wiring-version").is_none());
        assert_eq!(
            serde_json::from_value::<ServingAttachment>(route.clone()).unwrap(),
            route_attachment()
        );

        let wiring = serde_json::to_value(attachment()).expect("attachment serializes");
        let mut both = route.clone();
        both["wiring-id"] = wiring["wiring-id"].clone();
        both["wiring-version"] = wiring["wiring-version"].clone();
        let mut neither = route.clone();
        neither.as_object_mut().unwrap().remove("component");
        neither.as_object_mut().unwrap().remove("operation");
        let mut half = route;
        half.as_object_mut().unwrap().remove("operation");
        for document in [both, neither, half] {
            let error = serde_json::from_value::<ServingAttachment>(document)
                .expect_err("an attachment must name exactly one target");
            assert!(error.to_string().contains("exactly one target"), "{error}");
        }
    }

    #[test]
    fn route_attachment_targets_are_exact() {
        let mut absent = route_attachment();
        absent.target = AttachmentTarget::Route {
            component: "http-request".into(),
            operation: "base:widget/list@1.0.0".into(),
        };
        absent.registered_operation = None;
        assert_eq!(
            ServingManifest::new(
                release(),
                components(),
                routes(),
                wirings(),
                BTreeMap::from([("widget-get".to_string(), absent)]),
                BTreeMap::new(),
            ),
            Err(CatalogIdentityError::UnresolvableManifestRoute {
                package_id: "base".into(),
                component: "http-request".into(),
                operation: "base:widget/list@1.0.0".into(),
            })
        );

        let mut other_package = route_attachment();
        other_package.package_id = "overlay".into();
        other_package.registered_operation = None;
        assert!(matches!(
            ServingManifest::new(
                release(),
                components(),
                routes(),
                wirings(),
                BTreeMap::from([("widget-get".to_string(), other_package)]),
                BTreeMap::new(),
            ),
            Err(CatalogIdentityError::UnresolvableManifestRoute { .. })
        ));

        let mut mismatched = route_attachment();
        mismatched.registered_operation = Some("base:widget/list@1.0.0".into());
        let error = ServingManifest::new(
            release(),
            components(),
            routes(),
            wirings(),
            BTreeMap::from([("widget-get".to_string(), mismatched)]),
            BTreeMap::new(),
        )
        .expect_err("a route attachment cannot grant another operation");
        assert!(
            error
                .to_string()
                .contains("differs from its route operation")
        );
    }

    #[test]
    fn a_route_resolves_to_one_release_component_export() {
        let route = routes().pop_first().expect("fixture has a route");
        let unexported = ServingRoute {
            operation: "base:widget/list@1.0.0".into(),
            ..route.clone()
        };
        let handler = ServingRoute {
            type_: OperationType::EventHandler,
            ..route.clone()
        };
        let duplicate = BTreeSet::from([
            route.clone(),
            ServingRoute {
                type_: OperationType::Query,
                ..route
            },
        ]);
        for (routes, refusal) in [
            (
                BTreeSet::from([unexported]),
                "resolves to 0 release components",
            ),
            (BTreeSet::from([handler]), "an event handler is not a route"),
            (duplicate, "occurs more than once"),
        ] {
            let error = ServingManifest::new(
                release(),
                components(),
                routes,
                wirings(),
                BTreeMap::new(),
                BTreeMap::new(),
            )
            .expect_err("an inexact route must refuse");
            assert!(error.to_string().contains(refusal), "{error}");
        }
    }

    #[test]
    fn an_attachment_id_names_one_attachment_across_the_manifest() {
        let mut manifest = manifest();
        let duplicate = manifest.workflow.attachments["orders"].clone();
        manifest
            .workflow
            .attachments
            .insert("widget-get".into(), duplicate);
        let error = ServingManifest::from_canonical_bytes(&manifest.canonical_bytes())
            .expect_err("one attachment id cannot name a route and a wiring");
        assert!(error.to_string().contains("occurs both"), "{error}");
    }

    #[test]
    fn a_release_with_no_wiring_omits_the_workflow_section() {
        let manifest = ServingManifest::new(
            release(),
            components(),
            routes(),
            BTreeSet::new(),
            BTreeMap::from([("widget-get".to_string(), route_attachment())]),
            BTreeMap::new(),
        )
        .expect("a route-only release is valid");
        let document = serde_json::to_value(&manifest).expect("manifest serializes");
        assert!(document.get("workflow").is_none());

        let mut empty = document;
        empty["workflow"] = serde_json::json!({"wirings": []});
        assert_eq!(
            ServingManifest::from_canonical_bytes(&wamn_execution_contract::canonical_json_bytes(
                &empty
            )),
            Err(CatalogIdentityError::NonCanonicalJson)
        );
    }

    #[test]
    fn the_configmap_name_is_a_dns_1123_subdomain() {
        let name = release_manifest_configmap_name(COMPONENT_A).expect("digest names a map");
        assert_eq!(
            name,
            "release-manifest-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }
}
