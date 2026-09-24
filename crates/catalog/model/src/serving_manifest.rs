//! The immutable release-serving manifest mounted by every serving process.
//!
//! Format 3 closes over exact package membership, component digests, routes,
//! wiring definitions, the permissions and SQL statements of each export's
//! call graph, attachments, and registrations. Publish folds each call graph,
//! because an application's components compose at build into one component. A route calls one component
//! export; a wiring is a graph the router walks. It contains no flow or
//! execution-plan identity. Producers must source every member from current
//! catalog records; this model intentionally provides no legacy-plan
//! conversion. Attachment authentication uses a closed modes list, without a
//! scalar-mode fallback.
//!
//! The document identity is the SHA-256 of its RFC 8785 canonical JSON. Sets and
//! maps make each collection's order deterministic, while
//! [`ServingManifest::from_canonical_bytes`] rejects bytes whose order or JSON
//! encoding differs from that canonical representation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AdmittedComponent, AdmittedComponentOperation, ArtifactHash, AttachmentKind,
    CatalogIdentityError, ComponentOperationDependency, ComponentSqlStatement, DefinitionHash,
    EffectiveReleaseId, HASH_PREFIX, ManifestDigest, PackageCoordinate,
    package::validate_canonical_operation_for_package, validate_digest, validate_text,
};

/// The only serving-manifest format admitted by this revision.
pub const SERVING_MANIFEST_FORMAT_VERSION: u32 = 3;

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
pub enum AttachmentAuthPolicy {
    None,
    Pat,
    Session,
    PatAndSession,
}

impl AttachmentAuthPolicy {
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
/// `{"modes":["session"]}`, and `{"modes":["pat","session"]}`. Unknown
/// fields, scalar modes, duplicates, and other list orders are refused.
pub fn parse_attachment_auth_policy(policy: &Value) -> Option<AttachmentAuthPolicy> {
    let object = policy.as_object()?;
    if object.len() != 1 {
        return None;
    }
    let modes = object.get("modes")?.as_array()?;
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

/// Byte ceiling shared by the manifest mint and mount reader.
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

/// The release coordinate and environment this manifest projects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingRelease {
    pub tenant_id: String,
    pub effective_release_id: EffectiveReleaseId,
    pub environment: String,
    pub packages: BTreeSet<PackageCoordinate>,
}

/// One immutable component artifact in the release closure.
///
/// One operation exported by a release component.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingComponentOperation {
    /// Base-owned typed pre-commit import, bound only by an admitted caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_commit: Option<String>,
    /// Explicit application permission identity. Palette exports carry none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_operation: Option<String>,
    /// Every registered operation in this export's call graph, its own included.
    ///
    /// Publish computes the set. The caller must hold each member.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub permissions: BTreeSet<String>,
    /// Require a fresh originating credential. Publish sets it when any
    /// operation in the call graph requires one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fresh_only: bool,
    /// Canonical JSON for this registered operation's admitted committed result schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_result_schema: Option<String>,
    /// The local participant that the base selects inside this export's transaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participant: Option<String>,
    /// Exact SQL available while this export is active: the union over its call graph.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub statements: BTreeMap<String, ComponentSqlStatement>,
}

impl ServingComponentOperation {
    /// Resolve one release-pinned statement inside this operation's authority.
    pub fn statement(&self, digest: &str) -> Option<&ComponentSqlStatement> {
        self.statements.get(digest)
    }

    /// Whether this folded operation carries the admitted operation's own facts.
    ///
    /// Publish only adds to an operation when it folds the call graph, so the
    /// loaded admitted fact must be contained in the released operation.
    pub fn carries(&self, admitted: &AdmittedComponentOperation) -> bool {
        let schema = admitted.committed_result_schema.as_ref().map(|schema| {
            String::from_utf8(wamn_execution_contract::canonical_json_bytes(
                &schema.schema,
            ))
            .expect("canonical JSON uses UTF-8")
        });
        self.pre_commit == admitted.pre_commit
            && self.registered_operation == admitted.registered_operation
            && self.committed_result_schema == schema
            && (self.fresh_only || !admitted.fresh_only)
            && admitted
                .registered_operation
                .as_ref()
                .is_none_or(|registered| self.permissions.contains(registered))
            && admitted
                .statements
                .iter()
                .all(|(digest, statement)| self.statements.get(digest) == Some(statement))
    }
}

/// One immutable component artifact in the release closure.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingComponent {
    pub package_id: String,
    pub component: String,
    pub interface_version: String,
    pub digest: ArtifactHash,
    pub operations: BTreeMap<String, ServingComponentOperation>,
}

impl ServingComponent {
    /// Project one admitted component and fold each export's call graph.
    ///
    /// An application's components compose at build into one component, so a
    /// dependency is a component-model call inside these bytes, with no host
    /// between. Publish decides the authority of that call here, once:
    /// `resolve` returns the admitted fact of each dependency's base. Every
    /// export then carries the union of the permissions and SQL statements of
    /// its call graph, the fresh-credential rule of any callee, and the
    /// participant that the base selects.
    pub fn project<'a>(
        fact: &'a AdmittedComponent,
        resolve: &dyn Fn(&ComponentOperationDependency) -> Option<&'a AdmittedComponent>,
    ) -> Result<Self, CatalogIdentityError> {
        let digest = ArtifactHash::parse(fact.component_digest.clone())?;
        let mut operations = BTreeMap::new();
        for (name, operation) in &fact.operations {
            let mut folded = ServingComponentOperation {
                pre_commit: operation.pre_commit.clone(),
                registered_operation: operation.registered_operation.clone(),
                permissions: BTreeSet::new(),
                fresh_only: false,
                committed_result_schema: operation.committed_result_schema.as_ref().map(|schema| {
                    String::from_utf8(wamn_execution_contract::canonical_json_bytes(
                        &schema.schema,
                    ))
                    .expect("canonical JSON uses UTF-8")
                }),
                participant: None,
                statements: BTreeMap::new(),
            };
            fold_call_graph(fact, name, resolve, &mut Vec::new(), &mut folded)?;
            operations.insert(name.clone(), folded);
        }
        Ok(Self {
            package_id: fact.scope.package_id.clone(),
            component: fact.component.clone(),
            interface_version: fact.interface_version.clone(),
            digest,
            operations,
        })
    }
}

/// Merge one operation and everything it calls into `folded`.
fn fold_call_graph<'a>(
    fact: &'a AdmittedComponent,
    operation_name: &str,
    resolve: &dyn Fn(&ComponentOperationDependency) -> Option<&'a AdmittedComponent>,
    path: &mut Vec<(String, String)>,
    folded: &mut ServingComponentOperation,
) -> Result<(), CatalogIdentityError> {
    let key = (fact.component_digest.clone(), operation_name.to_owned());
    if path.contains(&key) {
        return invalid(format!(
            "operation {operation_name:?} calls itself through its dependencies"
        ));
    }
    let operation =
        fact.operation(operation_name)
            .ok_or_else(|| CatalogIdentityError::InvalidDefinition {
                message: format!(
                    "component {:?} does not export operation {operation_name:?}",
                    fact.component
                ),
            })?;
    path.push(key);
    folded
        .permissions
        .extend(operation.registered_operation.iter().cloned());
    folded.fresh_only |= operation.fresh_only;
    for (digest, statement) in &operation.statements {
        if let Some(previous) = folded.statements.insert(digest.clone(), statement.clone())
            && &previous != statement
        {
            return invalid(format!(
                "statement {digest:?} has two different facts in the call graph of one export"
            ));
        }
    }
    for dependency in &operation.dependencies {
        let base = resolve(dependency).ok_or_else(|| CatalogIdentityError::InvalidDefinition {
            message: format!(
                "component dependency {}@{} operation {:?} has no admitted fact",
                dependency.package, dependency.version, dependency.operation
            ),
        })?;
        fold_call_graph(base, &dependency.operation, resolve, path, folded)?;
        if dependency.participant.is_none()
            && base
                .operation(&dependency.operation)
                .is_some_and(|called| called.pre_commit_required)
        {
            return invalid(format!(
                "operation {:?} needs a pre-commit participant; the overlay declares none",
                dependency.operation
            ));
        }
        if let Some(participant) = &dependency.participant {
            if folded.participant.replace(participant.clone()).is_some() {
                return invalid(format!(
                    "the call graph of operation {operation_name:?} selects more than one participant"
                ));
            }
            if base
                .operation(&dependency.operation)
                .is_none_or(|called| called.pre_commit.is_none())
            {
                return invalid("selected base operation declares no pre-commit interface");
            }
            if fact
                .operation(participant)
                .is_none_or(|local| !local.dependencies.is_empty())
            {
                return invalid(format!(
                    "participant {participant:?} must be a local operation that calls nothing"
                ));
            }
            fold_call_graph(fact, participant, resolve, path, folded)?;
        }
    }
    path.pop();
    Ok(())
}

/// One immutable wiring definition in the release closure.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingWiring {
    pub package_id: String,
    pub wiring_id: String,
    pub wiring_version: u32,
    pub graph_hash: DefinitionHash,
}

/// The contract kind of one application operation.
///
/// The values are the `kind` literals of the generated contract
/// `operation.json`, which is the only source of this fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Get,
    Query,
    Create,
    Update,
    Delete,
    Command,
    Projection,
    EventHandler,
}

impl OperationKind {
    /// The kinds that read without changing a record.
    pub const READ_KINDS: [Self; 3] = [Self::Get, Self::Query, Self::Projection];

    /// Whether this kind reads without changing a record.
    #[must_use]
    pub fn is_read(self) -> bool {
        Self::READ_KINDS.contains(&self)
    }
}

/// One component export that a route calls once, with no graph walk.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingRoute {
    pub package_id: String,
    pub component: String,
    pub operation: String,
    pub kind: OperationKind,
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

/// One release attachment targeting a route or an exact wiring version.
///
/// On the wire the target is flat: `component` and `operation`, or
/// `wiring-id` and `wiring-version`, never both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "AttachmentWire", into = "AttachmentWire")]
pub struct ServingAttachment {
    pub kind: AttachmentKind,
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
    kind: AttachmentKind,
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
            kind: wire.kind,
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
            kind: attachment.kind,
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

/// The delivery grain frozen for one release registration.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ServingRegistrationInput {
    #[default]
    Event,
    Batch,
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
}

/// The complete release document a serving process mounts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingManifest {
    pub format_version: u32,
    pub release: ServingRelease,
    pub components: BTreeSet<ServingComponent>,
    pub routes: BTreeSet<ServingRoute>,
    pub wirings: BTreeSet<ServingWiring>,
    pub attachments: BTreeMap<String, ServingAttachment>,
    pub registrations: BTreeMap<String, ServingRegistration>,
}

impl ServingManifest {
    /// Build and validate a manifest from authoritative current-record facts.
    ///
    /// This helper is test-only. The production mint serializes its projected
    /// facts and admits those exact bytes through [`Self::from_canonical_bytes`].
    #[cfg(feature = "test-util")]
    pub fn new(
        release: ServingRelease,
        components: BTreeSet<ServingComponent>,
        routes: BTreeSet<ServingRoute>,
        wirings: BTreeSet<ServingWiring>,
        attachments: BTreeMap<String, ServingAttachment>,
        registrations: BTreeMap<String, ServingRegistration>,
    ) -> Result<Self, CatalogIdentityError> {
        let manifest = Self {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release,
            components,
            routes,
            wirings,
            attachments,
            registrations,
        };
        manifest.validate()?;
        within_delivery_limit(manifest.canonical_bytes().len())?;
        Ok(manifest)
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

    /// Parse, validate, and admit only canonical format-3 bytes.
    ///
    /// The version is classified before the format-3 schema is decoded. This is
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
        let manifest = serde_json::from_value::<Self>(document).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("serving manifest JSON is invalid: {error}"),
            }
        })?;
        manifest.validate()?;
        if manifest.canonical_bytes() != bytes {
            return Err(CatalogIdentityError::NonCanonicalJson);
        }
        let digest = manifest.digest();
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
        validate_text(&self.release.tenant_id, "tenant-id")?;
        validate_text(&self.release.environment, "environment")?;
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

        for component in &self.components {
            validate_package_member(&package_versions, &component.package_id)?;
            validate_text(&component.component, "component")?;
            validate_text(&component.interface_version, "interface-version")?;
            if component.operations.is_empty() {
                return invalid("a serving component must export at least one operation");
            }
            for (export, operation) in &component.operations {
                validate_text(export, "component-operation")?;
                if let Some(pre_commit) = &operation.pre_commit {
                    validate_canonical_operation_for_package(
                        pre_commit,
                        &component.package_id,
                        package_versions[component.package_id.as_str()],
                    )?;
                    if pre_commit == export {
                        return invalid(
                            "pre-commit interface must differ from the operation export",
                        );
                    }
                }
                if operation.fresh_only && operation.permissions.is_empty() {
                    return invalid(format!(
                        "export {export:?} requires no permission, so it must not require a fresh credential"
                    ));
                }
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
                if let Some(participant) = &operation.participant
                    && !(operation.permissions.contains(participant)
                        && component.operations.get(participant).is_some_and(|local| {
                            local.registered_operation.as_deref() == Some(participant.as_str())
                        }))
                {
                    return invalid(format!(
                        "participant {participant:?} of export {export:?} is not a registered local operation in its permissions"
                    ));
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
                        crate::component_library::normalize_schema(value, "committed-result")
                            .and_then(|schema| {
                                crate::component_library::validate_committed_result_schema(
                                    operation.registered_operation.as_deref(),
                                    Some(&schema),
                                )
                            });
                    normalized.map_err(|error| CatalogIdentityError::InvalidDefinition {
                        message: format!("serving component operation {export:?} carries invalid committed-result facts: {error}"),
                    })?;
                }
                crate::component_library::validate_operation_statement_facts(
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
            if route.kind == OperationKind::EventHandler {
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
                        && component.component == route.component
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

        let mut targets = BTreeSet::new();
        for wiring in &self.wirings {
            validate_package_member(&package_versions, &wiring.package_id)?;
            validate_text(&wiring.wiring_id, "wiring-id")?;
            if wiring.wiring_version == 0 {
                return Err(CatalogIdentityError::ZeroVersion {
                    field: "wiring-version",
                });
            }
            if !targets.insert((
                wiring.package_id.as_str(),
                wiring.wiring_id.as_str(),
                wiring.wiring_version,
            )) {
                return invalid("a wiring identity-version pair occurs more than once");
            }
        }

        for (attachment_id, attachment) in &self.attachments {
            validate_text(attachment_id, "attachment-id")?;
            validate_package_member(&package_versions, &attachment.package_id)?;
            match &attachment.target {
                AttachmentTarget::Route {
                    component,
                    operation,
                } => {
                    validate_route_target(
                        &routes,
                        attachment_id,
                        attachment,
                        component,
                        operation,
                    )?;
                }
                AttachmentTarget::Wiring {
                    wiring_id,
                    wiring_version,
                } => validate_wiring_target(
                    &targets,
                    &attachment.package_id,
                    wiring_id,
                    *wiring_version,
                )?,
            }
            if !attachment.definition.is_object() {
                return invalid("attachment definition must be a JSON object");
            }
            let auth_policy =
                parse_attachment_auth_policy(&attachment.auth_policy).ok_or_else(|| {
                    CatalogIdentityError::InvalidAttachmentAuthPolicy {
                        attachment_id: attachment_id.clone(),
                    }
                })?;
            if auth_policy == AttachmentAuthPolicy::None
                && attachment.registered_operation.is_some()
            {
                return Err(CatalogIdentityError::UnauthenticatedRegisteredOperation {
                    attachment_id: attachment_id.clone(),
                });
            }
            if contains_retired_identity(&attachment.definition)
                || contains_retired_identity(&attachment.auth_policy)
            {
                return invalid("attachment configuration carries retired flow or plan identity");
            }
            validate_registered_operation(
                &package_versions,
                &attachment.package_id,
                attachment.registered_operation.as_deref(),
            )?;
        }

        for (registration_id, registration) in &self.registrations {
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
    attachment: &ServingAttachment,
    component: &str,
    operation: &str,
) -> Result<(), CatalogIdentityError> {
    validate_text(component, "component")?;
    validate_text(operation, "operation")?;
    if attachment.kind == AttachmentKind::Cron {
        return invalid(format!(
            "cron attachment {attachment_id:?} cannot target a route"
        ));
    }
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

    const COMPONENT_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const COMPONENT_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const GRAPH_A: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const GRAPH_B: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
    const DEFINITION: &str =
        "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    fn artifact_hash(value: &str) -> ArtifactHash {
        ArtifactHash::parse(value).expect("fixture artifact hash is canonical")
    }

    fn definition_hash(value: &str) -> DefinitionHash {
        DefinitionHash::parse(value).expect("fixture definition hash is canonical")
    }

    fn release() -> ServingRelease {
        ServingRelease {
            tenant_id: "t1".into(),
            effective_release_id: EffectiveReleaseId::new(7).unwrap(),
            environment: "prod".into(),
            packages: BTreeSet::from([
                PackageCoordinate::new("base", "1.0.0").unwrap(),
                PackageCoordinate::new("overlay", "3.0.0").unwrap(),
            ]),
        }
    }

    fn components() -> BTreeSet<ServingComponent> {
        BTreeSet::from([
            ServingComponent {
                package_id: "overlay".into(),
                component: "transform".into(),
                interface_version: "0.1".into(),
                digest: artifact_hash(COMPONENT_B),
                operations: BTreeMap::from([(
                    "overlay:transform/map@3.0.0".into(),
                    ServingComponentOperation {
                        pre_commit: None,
                        committed_result_schema: None,
                        fresh_only: false,
                        registered_operation: Some("overlay:transform/map@3.0.0".into()),
                        permissions: BTreeSet::from([
                            "base:widget/get@1.0.0".into(),
                            "overlay:transform/map@3.0.0".into(),
                        ]),
                        participant: None,
                        statements: BTreeMap::new(),
                    },
                )]),
            },
            ServingComponent {
                package_id: "base".into(),
                component: "http-request".into(),
                interface_version: "0.1".into(),
                digest: artifact_hash(COMPONENT_A),
                operations: BTreeMap::from([(
                    "base:widget/get@1.0.0".into(),
                    ServingComponentOperation {
                        pre_commit: None,
                        committed_result_schema: None,
                        fresh_only: false,
                        registered_operation: Some("base:widget/get@1.0.0".into()),
                        permissions: BTreeSet::from(["base:widget/get@1.0.0".into()]),
                        participant: None,
                        statements: BTreeMap::new(),
                    },
                )]),
            },
        ])
    }

    fn wirings() -> BTreeSet<ServingWiring> {
        BTreeSet::from([
            ServingWiring {
                package_id: "overlay".into(),
                wiring_id: "shipping".into(),
                wiring_version: 2,
                graph_hash: definition_hash(GRAPH_B),
            },
            ServingWiring {
                package_id: "base".into(),
                wiring_id: "orders".into(),
                wiring_version: 3,
                graph_hash: definition_hash(GRAPH_A),
            },
        ])
    }

    fn routes() -> BTreeSet<ServingRoute> {
        BTreeSet::from([ServingRoute {
            package_id: "base".into(),
            component: "http-request".into(),
            operation: "base:widget/get@1.0.0".into(),
            kind: OperationKind::Get,
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
            kind: AttachmentKind::Http,
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

    fn admitted_operation(
        registered: &str,
        statement: Option<&str>,
        dependencies: Vec<ComponentOperationDependency>,
    ) -> crate::AdmittedComponentOperation {
        crate::AdmittedComponentOperation {
            pre_commit: None,
            pre_commit_required: false,
            registered_operation: Some(registered.into()),
            fresh_only: false,
            committed_result_schema: None,
            dependencies,
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
    fn projection_folds_each_call_graph_once_at_publish() {
        const OVERLAY: &str = "overlay:receiving/record@3.0.0";
        const PARTICIPANT: &str = "overlay:receiving/record-participant@3.0.0";
        const BASE: &str = "base:receiving/record@1.0.0";
        let mut base_operation =
            admitted_operation(BASE, Some("INSERT INTO receipt DEFAULT VALUES"), Vec::new());
        base_operation.pre_commit = Some("base:receiving/record-pre-commit@1.0.0".into());
        base_operation.fresh_only = true;
        let base = admitted("base", "1.0.0", COMPONENT_A, vec![(BASE, base_operation)]);
        let dependency = ComponentOperationDependency {
            participant: Some(PARTICIPANT.into()),
            package: "base".into(),
            version: "1.0.0".into(),
            digest: COMPONENT_A.into(),
            operation: BASE.into(),
        };
        let overlay = admitted(
            "overlay",
            "3.0.0",
            COMPONENT_B,
            vec![
                (
                    OVERLAY,
                    admitted_operation(OVERLAY, None, vec![dependency.clone()]),
                ),
                (
                    PARTICIPANT,
                    admitted_operation(
                        PARTICIPANT,
                        Some("INSERT INTO inspection DEFAULT VALUES"),
                        Vec::new(),
                    ),
                ),
            ],
        );
        let resolve = |_: &ComponentOperationDependency| Some(&base);
        let projected = ServingComponent::project(&overlay, &resolve).expect("the graph folds");
        let entry = &projected.operations[OVERLAY];
        assert_eq!(
            entry.permissions,
            BTreeSet::from([BASE.into(), OVERLAY.into(), PARTICIPANT.into()])
        );
        assert!(
            entry.fresh_only,
            "the entry inherits the callee's fresh-credential rule"
        );
        assert_eq!(entry.participant.as_deref(), Some(PARTICIPANT));
        assert_eq!(
            entry.statements.len(),
            2,
            "the entry holds the SQL of its whole graph"
        );
        let participant = &projected.operations[PARTICIPANT];
        assert_eq!(
            participant.permissions,
            BTreeSet::from([PARTICIPANT.into()])
        );
        assert!(participant.participant.is_none() && !participant.fresh_only);

        let unresolved = |_: &ComponentOperationDependency| None;
        let error = ServingComponent::project(&overlay, &unresolved)
            .expect_err("an unresolved dependency was folded");
        assert!(error.to_string().contains("has no admitted fact"));

        let mut cyclic_base = base.clone();
        cyclic_base.operations.get_mut(BASE).unwrap().dependencies =
            vec![ComponentOperationDependency {
                participant: None,
                package: "overlay".into(),
                version: "3.0.0".into(),
                digest: COMPONENT_B.into(),
                operation: OVERLAY.into(),
            }];
        let cyclic = |dependency: &ComponentOperationDependency| {
            Some(if dependency.package == "base" {
                &cyclic_base
            } else {
                &overlay
            })
        };
        let error = ServingComponent::project(&overlay, &cyclic)
            .expect_err("a call graph cycle was folded");
        assert!(error.to_string().contains("calls itself"));
    }

    #[test]
    fn publish_refuses_a_required_pre_commit_without_a_participant() {
        const OVERLAY: &str = "overlay:receiving/record@3.0.0";
        const BASE: &str = "base:receiving/record@1.0.0";
        let mut base_operation = admitted_operation(BASE, None, Vec::new());
        base_operation.pre_commit = Some("base:receiving/record-pre-commit@1.0.0".into());
        let mut base = admitted("base", "1.0.0", COMPONENT_A, vec![(BASE, base_operation)]);
        let dependency = ComponentOperationDependency {
            participant: None,
            package: "base".into(),
            version: "1.0.0".into(),
            digest: COMPONENT_A.into(),
            operation: BASE.into(),
        };
        let overlay = admitted(
            "overlay",
            "3.0.0",
            COMPONENT_B,
            vec![(OVERLAY, admitted_operation(OVERLAY, None, vec![dependency]))],
        );

        let optional = base.clone();
        let resolve = |_: &ComponentOperationDependency| Some(&optional);
        let projected =
            ServingComponent::project(&overlay, &resolve).expect("an optional slot folds");
        assert!(projected.operations[OVERLAY].participant.is_none());

        base.operations.get_mut(BASE).unwrap().pre_commit_required = true;
        let resolve = |_: &ComponentOperationDependency| Some(&base);
        let error = ServingComponent::project(&overlay, &resolve)
            .expect_err("a required slot without a participant was published");
        assert_eq!(
            error.to_string(),
            format!("operation {BASE:?} needs a pre-commit participant; the overlay declares none")
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

    #[test]
    fn only_canonical_format_three_bytes_are_admitted() {
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

    #[test]
    fn statement_lookup_is_operation_scoped_and_revalidates_exact_bytes() {
        let operation = "base:widget/get@1.0.0";
        let sql = "SELECT row_version FROM widget WHERE id = $1";
        let digest = crate::digest(sql.as_bytes());
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
                digest.clone(),
                ComponentSqlStatement {
                    name: "get".into(),
                    path: "generated/sql/widget/get.sql".into(),
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
            .statement(&digest);
        assert!(admitted.is_some());
        assert!(
            manifest
                .components
                .iter()
                .find(|component| component.package_id == "overlay")
                .expect("overlay component remains present")
                .operations["overlay:transform/map@3.0.0"]
                .statement(&digest)
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
            .get_mut(&digest)
            .expect("fixture has a statement")
            .sql
            .push(' ');
        broken.insert(base);
        let error = ServingManifest::new(
            release(),
            broken,
            routes(),
            wirings(),
            BTreeMap::from([("orders".to_string(), attachment())]),
            BTreeMap::from([("overlay::orders-changed".to_string(), registration())]),
        )
        .expect_err("statement bytes may not drift from their digest");
        assert!(error.to_string().contains("invalid statement facts"));
    }

    #[test]
    fn unsupported_formats_are_typed_refusals_not_compatibility_arms() {
        for version in [0, 1, 2, 4] {
            let unsupported = serde_json::to_vec(&serde_json::json!({
                "format-version": version,
                "release": {}
            }))
            .unwrap();
            let error = ServingManifest::from_canonical_bytes(&unsupported)
                .expect_err("only format three may enter the decoder");
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

        let mut cron = route_attachment();
        cron.kind = AttachmentKind::Cron;
        let error = ServingManifest::new(
            release(),
            components(),
            routes(),
            wirings(),
            BTreeMap::from([("widget-get".to_string(), cron)]),
            BTreeMap::new(),
        )
        .expect_err("a cron attachment cannot target a route");
        assert!(error.to_string().contains("cannot target a route"));
    }

    #[test]
    fn a_route_resolves_to_one_release_component_export() {
        let route = routes().pop_first().expect("fixture has a route");
        let unexported = ServingRoute {
            operation: "base:widget/list@1.0.0".into(),
            ..route.clone()
        };
        let handler = ServingRoute {
            kind: OperationKind::EventHandler,
            ..route.clone()
        };
        let duplicate = BTreeSet::from([
            route.clone(),
            ServingRoute {
                kind: OperationKind::Query,
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
    fn the_configmap_name_is_a_dns_1123_subdomain() {
        let name = release_manifest_configmap_name(COMPONENT_A).expect("digest names a map");
        assert_eq!(
            name,
            "release-manifest-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }
}
