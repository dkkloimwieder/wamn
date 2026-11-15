//! The immutable release-serving manifest mounted by every serving process.
//!
//! Format 11 closes over exact package membership, component digests, routes,
//! each export permission and route attachment.
//! Each component is its package, its admitted descriptor (the bytes of the
//! descriptor layer of its package artifact, docs/operations/building.md#reading-a-pushed-package
//! ). The descriptor is the one
//! owner of the component name, interface version and digest. An
//! application's components compose at build into one component. A route
//! calls one component export. Workflows belong to the environment.
//! Producers source every release member from current
//! catalog records; this model intentionally provides no legacy-plan
//! conversion. Attachment authentication uses a closed modes list, without a
//! scalar-mode fallback.
//!
//! The manifest names no project, environment, org, project, route host or
//! integer id, so one release hashes identically in every environment
//! (docs/architecture/overview.md#release-identity). A serving process takes
//! its project and environment from its own configuration.
//!
//! The document identity is the SHA-256 of its RFC 8785 canonical JSON. Sets and
//! maps make each collection's order deterministic, while
//! [`ServingManifest::from_canonical_bytes`] rejects bytes whose order or JSON
//! encoding differs from that canonical representation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AdmittedComponent, AttachmentType, CatalogIdentityError, ComponentConnection,
    ComponentDescriptor, HostAttachment, HostRouteSet, PackageCoordinate,
    package::validate_canonical_operation_for_package, validate_text,
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
pub fn release_manifest_configmap_name(manifest_digest: &wamn_digest::Digest) -> String {
    let hex = manifest_digest.hex();
    format!("{RELEASE_MANIFEST_CONFIGMAP_PREFIX}{hex}")
}

/// The exact package dependency closure of one application release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingRelease {
    pub packages: BTreeSet<PackageCoordinate>,
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
        )
            .cmp(&(
                &other.package_id,
                &other.runtime_schema,
                &other.descriptor.component,
                &other.descriptor.interface_version,
                &other.descriptor.component_digest,
            ))
            .then_with(|| {
                let bytes = |descriptor: &ComponentDescriptor| {
                    wamn_digest::canonical_json_bytes(
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
    /// call to a base is a component-model call inside these bytes. The host
    /// lends every member the same operation transaction. The descriptor is
    /// `fact` with its `connections`.
    pub fn project(fact: &AdmittedComponent, connections: Vec<ComponentConnection>) -> Self {
        Self {
            package_id: fact.target.package_id.clone(),
            runtime_schema: None,
            descriptor: ComponentDescriptor::new(fact.clone(), connections),
        }
    }
}

/// The contract type of one application operation.
///
/// The values are the `type` literals of the generated contract
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
    /// The types that read without changing a record.
    pub const READ_TYPES: [Self; 3] = [Self::Get, Self::Query, Self::Projection];

    /// Whether this type reads without changing a record.
    #[must_use]
    pub fn is_read(self) -> bool {
        Self::READ_TYPES.contains(&self)
    }

    /// The HTTP method of a route that calls an operation of this type.
    ///
    /// A read is a GET and every other type is a POST. The method follows from
    /// the type alone, so no author writes it (`docs/architecture/execution.md`).
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
    /// of another type reads none.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub reads: BTreeSet<ServingRelation>,
    /// The result field that carries the record revision of a `get`, from
    /// `record.revision_field` of its generated contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// The input request element field that carries the idempotency key, such as
    /// `value.idempotency_key`, from the generated input contract. A route
    /// without one keys its intents by the request element `request_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency: Option<String>,
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
}

/// One authored attachment targeting a route.
///
/// Publish reads this type from `publication/attachments.json` and converts it
/// into a [`RouteAttachment`] of the manifest.
///
/// On the wire the target is flat: `component` and `operation`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "AttachmentWire", into = "AttachmentWire")]
pub struct ServingAttachment {
    pub type_: AttachmentType,
    pub package_id: String,
    pub target: AttachmentTarget,
    pub definition_hash: wamn_digest::Digest,
    pub definition: Value,
    pub auth_policy: Value,
    /// Exact operation authority selected by this attachment. Attachments that
    /// do not invoke a package operation carry no token; callers never infer one
    /// from route, wiring, or component syntax.
    pub registered_operation: Option<crate::registered_operation::RegisteredOperation>,
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
    definition_hash: wamn_digest::Digest,
    definition: Value,
    auth_policy: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    registered_operation: Option<crate::registered_operation::RegisteredOperation>,
}

impl TryFrom<AttachmentWire> for ServingAttachment {
    type Error = String;

    fn try_from(wire: AttachmentWire) -> Result<Self, Self::Error> {
        let target = AttachmentTarget::Route {
            component: wire.component.ok_or("attachment component is required")?,
            operation: wire.operation.ok_or("attachment operation is required")?,
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
        let AttachmentTarget::Route {
            component,
            operation,
        } = attachment.target;
        Self {
            type_: attachment.type_,
            package_id: attachment.package_id,
            component: Some(component),
            operation: Some(operation),
            definition_hash: attachment.definition_hash,
            definition: attachment.definition,
            auth_policy: attachment.auth_policy,
            registered_operation: attachment.registered_operation,
        }
    }
}

impl ServingAttachment {
    /// Project authored route attachments into the manifest.
    pub fn routes(authored: BTreeMap<String, Self>) -> BTreeMap<String, RouteAttachment> {
        authored
            .into_iter()
            .map(|(id, attachment)| {
                let AttachmentTarget::Route {
                    component,
                    operation,
                } = attachment.target;
                (
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
                )
            })
            .collect()
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

impl From<AttachmentRef<'_>> for ServingAttachment {
    fn from(attachment: AttachmentRef<'_>) -> Self {
        match attachment {
            AttachmentRef::Route(attachment) => attachment.clone().into(),
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
    pub definition_hash: wamn_digest::Digest,
    pub definition: Value,
    pub auth_policy: Value,
    /// Exact operation authority selected by this attachment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registered_operation: Option<crate::registered_operation::RegisteredOperation>,
}

/// One borrowed route attachment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AttachmentRef<'a> {
    Route(&'a RouteAttachment),
}

impl<'a> AttachmentRef<'a> {
    pub fn attachment_type(self) -> AttachmentType {
        match self {
            Self::Route(attachment) => attachment.type_,
        }
    }

    pub fn package_id(self) -> &'a str {
        match self {
            Self::Route(attachment) => &attachment.package_id,
        }
    }

    pub fn definition(self) -> &'a Value {
        match self {
            Self::Route(attachment) => &attachment.definition,
        }
    }

    pub fn auth_policy(self) -> &'a Value {
        match self {
            Self::Route(attachment) => &attachment.auth_policy,
        }
    }

    pub fn registered_operation(
        self,
    ) -> Option<&'a crate::registered_operation::RegisteredOperation> {
        match self {
            Self::Route(attachment) => attachment.registered_operation.as_ref(),
        }
    }

    /// The route that this attachment calls.
    pub fn target(self) -> AttachmentTarget {
        match self {
            Self::Route(attachment) => AttachmentTarget::Route {
                component: attachment.component.clone(),
                operation: attachment.operation.clone(),
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

    pub fn registered_operation(
        self,
    ) -> Option<&'a crate::registered_operation::RegisteredOperation> {
        match self {
            Self::Authored(attachment) => attachment.registered_operation(),
            Self::Host(attachment) => attachment.registered_operation(),
        }
    }
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
    pub digest: wamn_digest::Digest,
}

/// The published HTTP component and the source identities that fix its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ServingComposedEntry {
    /// `entries@sha256:<manifest digest>`, relative to the component artifact
    /// base that each host resolves members against, or a local absolute `file:`.
    pub reference: String,
    pub route_digest: wamn_digest::Digest,
    /// The explicitly named application whose bytes compose the entry.
    pub application: ServingEntryMember,
    pub members: BTreeSet<ServingEntryMember>,
}

/// The complete release document for one application.
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
    /// The fixed host route sets this release serves
    /// (docs/architecture/execution.md#host-run-routes). The platform builds
    /// their routes, so the manifest names only the sets.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub host_routes: BTreeSet<HostRouteSet>,
}

impl ServingManifest {
    /// Resolve the manifest's admitted component facts for one project environment.
    pub fn admitted_components(
        &self,
        project_env_key: &str,
    ) -> Result<Vec<AdmittedComponent>, CatalogIdentityError> {
        self.components
            .iter()
            .map(|component| {
                let version = if component.package_id == crate::PALETTE_PACKAGE_ID {
                    crate::PALETTE_PACKAGE_VERSION
                        .parse()
                        .expect("fixed palette package version")
                } else {
                    self.release
                        .packages
                        .iter()
                        .find(|package| package.package_id() == component.package_id)
                        .ok_or_else(|| CatalogIdentityError::InvalidDefinition {
                            message: "component package absent from release".to_owned(),
                        })?
                        .package_version()
                        .clone()
                };
                let target = crate::ComponentPackageCoordinate::new(
                    project_env_key,
                    &component.package_id,
                    version,
                )?;
                Ok(component.descriptor.clone().into_admitted(target).component)
            })
            .collect()
    }

    /// Whether this operation returns short-lived file capabilities.
    pub fn signs_file_results(&self, package: &str, component: &str, operation: &str) -> bool {
        self.components
            .iter()
            .find(|served| served.package_id == package && served.descriptor.component == component)
            .and_then(|served| served.descriptor.operations.get(operation))
            .is_some_and(|operation| {
                operation
                    .output_ports
                    .iter()
                    .any(|port| port.schema.schema.get(crate::SIGNED_FILE_READS).is_some())
            })
    }

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
        attachments: BTreeMap<String, ServingAttachment>,
    ) -> Result<Self, CatalogIdentityError> {
        let attachments = ServingAttachment::routes(attachments);
        let manifest = Self {
            format_version: SERVING_MANIFEST_FORMAT_VERSION,
            release,
            components,
            composed_entry: None,
            routes,
            attachments,
            host_routes: BTreeSet::new(),
        };
        manifest.validate()?;
        within_delivery_limit(manifest.canonical_bytes().len())?;
        Ok(manifest)
    }

    /// One route attachment by id.
    pub fn attachment(&self, id: &str) -> Option<AttachmentRef<'_>> {
        self.attachments.get(id).map(AttachmentRef::Route)
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

    /// The explicitly named application member of the composed entry.
    pub fn composing_member(&self) -> Option<&ServingComponent> {
        let application = &self.composed_entry.as_ref()?.application;
        self.components.iter().find(|component| {
            component.package_id == application.package_id
                && component.descriptor.component == application.component
                && component.descriptor.component_digest == application.digest
        })
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

    /// Every route attachment of the release, in id order.
    pub fn every_attachment(&self) -> impl Iterator<Item = (&str, AttachmentRef<'_>)> {
        self.attachments
            .iter()
            .map(|(id, attachment)| (id.as_str(), AttachmentRef::Route(attachment)))
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
        wamn_digest::canonical_json_bytes(&self.as_value())
    }

    /// The content digest over [`Self::canonical_bytes`].
    pub fn digest(&self) -> wamn_digest::Digest {
        wamn_digest::canonical_json_sha256(&self.as_value())
    }

    /// Parse, validate, and admit only canonical format-11 bytes.
    ///
    /// The version is classified before the format-11 schema is decoded. This is
    /// what makes an unsupported mount an explicit typed refusal rather than a
    /// generic unknown-field parse error, and it deliberately provides no
    /// dual-version tolerance.
    pub fn from_canonical_bytes(
        bytes: &[u8],
    ) -> Result<(Self, wamn_digest::Digest), CatalogIdentityError> {
        within_delivery_limit(bytes.len())?;
        let document = serde_json::from_slice::<Value>(bytes).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("serving manifest JSON is invalid: {error}"),
            }
        })?;
        validate_format_version(&document)?;
        if document
            .get("composed-entry")
            .and_then(Value::as_object)
            .is_some_and(|entry| !entry.contains_key("application"))
        {
            return invalid("the composed entry is missing composed-entry.application");
        }
        let manifest = serde_json::from_value::<Self>(document.clone()).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("serving manifest JSON is invalid: {error}"),
            }
        })?;
        manifest.validate()?;
        if manifest.as_value() != document || wamn_digest::canonical_json_bytes(&document) != bytes
        {
            return Err(CatalogIdentityError::NonCanonicalJson);
        }
        let digest = wamn_digest::canonical_json_sha256(&document);
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
            if package.package_id() == crate::PALETTE_PACKAGE_ID {
                return invalid("the platform palette is not an application package");
            }

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

            if entry.members.is_empty() {
                return invalid("a composed entry must name its application members");
            }
            if !entry.members.contains(&entry.application) {
                return invalid("the named application is outside the composed entry members");
            }
            let mut identities = BTreeSet::new();
            for member in &entry.members {
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
            if component.package_id == crate::PALETTE_PACKAGE_ID {
                if component.runtime_schema.is_some()
                    || component.descriptor.operations.values().any(|operation| {
                        operation.registered_operation.is_some() || operation.host_transaction
                    })
                {
                    return invalid(
                        "platform palette components cannot own application operations or SQL",
                    );
                }
            } else {
                validate_package_member(&package_versions, &component.package_id)?;
            }
            let descriptor = &component.descriptor;
            validate_text(&descriptor.component, "component")?;
            validate_text(&descriptor.interface_version, "interface-version")?;

            if component.descriptor.operations.is_empty() {
                return invalid("a serving component must export at least one operation");
            }
            for operation in descriptor.operations.values() {
                crate::signed_file_reads(operation).map_err(|error| {
                    CatalogIdentityError::InvalidDefinition {
                        message: error.to_string(),
                    }
                })?;
            }
            for (export, operation) in &component.descriptor.operations {
                validate_text(export, "component-operation")?;
                validate_registered_operation(
                    &package_versions,
                    &component.package_id,
                    operation.registered_operation.as_ref(),
                )?;
                if operation
                    .registered_operation
                    .as_ref()
                    .is_some_and(|registered| registered.as_str() != export)
                {
                    return invalid(format!(
                        "component export {export:?} and registered operation {:?} differ",
                        operation.registered_operation
                    ));
                }
                crate::component_facts::validate_committed_result_schema(
                    operation.registered_operation.as_ref(),
                    operation.committed_result_schema.as_ref(),
                ).map_err(|error| CatalogIdentityError::InvalidDefinition {
                    message: format!("serving component operation {export:?} carries invalid committed-result facts: {error}"),
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
                        && component
                            .descriptor
                            .operations
                            .contains_key(&route.operation)
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
                attachment.registered_operation.as_ref(),
            )?;
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
        .as_ref()
        .is_some_and(|registered| registered.as_str() != operation)
    {
        return invalid(format!(
            "attachment {attachment_id:?} registered operation {:?} differs from its route operation {operation:?}",
            attachment.registered_operation
        ));
    }
    Ok(())
}

/// The tests that every attachment passes, whatever it targets.
fn validate_attachment(
    package_versions: &BTreeMap<&str, &crate::package_version::PackageVersion>,
    attachment_id: &str,
    package_id: &str,
    definition: &Value,
    auth_policy: &Value,
    registered_operation: Option<&crate::registered_operation::RegisteredOperation>,
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

fn validate_package_member(
    package_versions: &BTreeMap<&str, &crate::package_version::PackageVersion>,
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
    package_versions: &BTreeMap<&str, &crate::package_version::PackageVersion>,
    package_id: &str,
    operation: Option<&crate::registered_operation::RegisteredOperation>,
) -> Result<(), CatalogIdentityError> {
    let Some(operation) = operation else {
        return Ok(());
    };
    let package_version = package_versions
        .get(package_id)
        .expect("package membership was validated before operation identity");
    validate_canonical_operation_for_package(operation, package_id, package_version)
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
    const DEFINITION: &str =
        "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

    fn release() -> ServingRelease {
        ServingRelease {
            packages: BTreeSet::from([
                PackageCoordinate::new("base", "1.0.0".parse().unwrap()).unwrap(),
                PackageCoordinate::new("overlay", "3.0.0".parse().unwrap()).unwrap(),
            ]),
        }
    }

    fn components() -> BTreeSet<ServingComponent> {
        [
            (
                "overlay",
                "transform",
                COMPONENT_B,
                "overlay:transform/map@3.0.0",
            ),
            ("base", "http-request", COMPONENT_A, "base:widget/get@1.0.0"),
        ]
        .into_iter()
        .map(|(package, component, digest, operation)| ServingComponent {
            runtime_schema: None,

            package_id: package.into(),
            descriptor: ComponentDescriptor {
                operations: BTreeMap::from([(
                    operation.into(),
                    crate::AdmittedComponentOperation {
                        registered_operation: Some(operation.parse().unwrap()),
                        committed_result_schema: None,
                        host_transaction: false,
                        input_ports: Vec::new(),
                        output_ports: Vec::new(),
                        parameters: Vec::new(),
                    },
                )]),
                ..ComponentDescriptor::named(component, "0.1", digest.parse().unwrap())
            },
        })
        .collect()
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
            target: AttachmentTarget::Route {
                component: "http-request".into(),
                operation: "base:widget/get@1.0.0".into(),
            },
            definition_hash: DEFINITION
                .parse()
                .expect("fixture definition hash is canonical"),
            definition: serde_json::json!({
                "id": "orders",
                "type": "http",
                "route": {"host": "*", "path": "/orders", "method": "POST"}
            }),
            auth_policy: serde_json::json!({"modes": ["pat"]}),
            registered_operation: Some("base:widget/get@1.0.0".parse().unwrap()),
        }
    }

    fn manifest() -> ServingManifest {
        ServingManifest::new(
            release(),
            components(),
            routes(),
            BTreeMap::from([
                ("orders".to_string(), attachment()),
                ("widget-get".to_string(), route_attachment()),
            ]),
        )
        .expect("fixture manifest is valid")
    }

    fn entry_for(manifest: &ServingManifest) -> ServingComposedEntry {
        ServingComposedEntry {
            reference: format!("entries@sha256:{}", "e".repeat(64)),
            route_digest: (format!("sha256:{}", "f".repeat(64)))
                .parse::<wamn_digest::Digest>()
                .unwrap(),
            application: ServingEntryMember {
                package_id: "overlay".into(),
                component: "transform".into(),
                digest: COMPONENT_B.parse().unwrap(),
            },
            members: manifest
                .components
                .iter()
                .map(|component| ServingEntryMember {
                    package_id: component.package_id.clone(),
                    component: component.descriptor.component.clone(),
                    digest: component.descriptor.component_digest,
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
            (format!("sha256:{}", "a".repeat(64)))
                .parse::<wamn_digest::Digest>()
                .unwrap();
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
        // Naming base changes the composing application.
        let entry = manifest.composed_entry.as_mut().unwrap();
        entry.application = entry
            .members
            .iter()
            .find(|member| member.package_id == "base")
            .unwrap()
            .clone();
        assert_eq!(manifest.composing_member().unwrap().package_id, "base");
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
        for change in [0, 2, 3, 4] {
            let mut manifest = manifest.clone();
            let mut entry = entry.clone();
            match change {
                0 => entry.reference = "registry.example/entry:latest".into(),
                2 => entry.members.clear(),
                3 => {
                    let mut member = entry.members.pop_first().unwrap();
                    member.digest = (format!("sha256:{}", "0".repeat(64)))
                        .parse::<wamn_digest::Digest>()
                        .unwrap();
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
    fn composed_entry_requires_a_named_application_in_its_members() {
        let mut manifest = manifest();
        manifest.composed_entry = Some(entry_for(&manifest));
        manifest.composed_entry.as_mut().unwrap().application.digest = COMPONENT_A.parse().unwrap();
        assert!(matches!(
            ServingManifest::from_canonical_bytes(&manifest.canonical_bytes()),
            Err(CatalogIdentityError::InvalidDefinition { .. })
        ));
    }

    #[test]
    fn format_eleven_requires_composed_entry_application() {
        let mut manifest = manifest();
        manifest.format_version = 11;
        manifest.composed_entry = Some(entry_for(&manifest));
        ServingManifest::from_canonical_bytes(&manifest.canonical_bytes())
            .expect("format eleven with a named application is admitted");
        let mut document = serde_json::to_value(&manifest).unwrap();
        document["composed-entry"]
            .as_object_mut()
            .unwrap()
            .remove("application");
        let error =
            ServingManifest::from_canonical_bytes(&wamn_digest::canonical_json_bytes(&document))
                .expect_err("composed-entry.application is required in format eleven");
        assert!(matches!(
            error,
            CatalogIdentityError::InvalidDefinition { .. }
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
    fn collection_insertion_order_cannot_reach_identity() {
        let forward = manifest();
        let reversed = ServingManifest::new(
            release(),
            components().into_iter().rev().collect(),
            routes().into_iter().rev().collect(),
            BTreeMap::from([
                ("widget-get".to_string(), route_attachment()),
                ("orders".to_string(), attachment()),
            ]),
        )
        .expect("reordered fixture is valid");

        assert_eq!(forward.canonical_bytes(), reversed.canonical_bytes());
        assert_eq!(forward.digest(), reversed.digest());
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

    #[test]
    fn unsupported_formats_are_typed_refusals_not_compatibility_arms() {
        for version in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 13] {
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
        }
    }

    #[test]
    fn registered_operations_match_the_containing_package_coordinate() {
        for operation in ["overlay:widget/get@3.0.0", "base:widget/get@2.0.0"] {
            let mut mismatched = attachment();
            mismatched.registered_operation = Some(operation.parse().unwrap());
            let _error = ServingManifest::new(
                release(),
                components(),
                routes(),
                BTreeMap::from([("orders".to_string(), mismatched)]),
            )
            .expect_err("attachment operation must match its selected package coordinate");
        }

        let mut mismatched_components = components();
        let mut component = mismatched_components
            .pop_first()
            .expect("fixture has a component");
        let component_operation = component
            .descriptor
            .operations
            .values_mut()
            .next()
            .expect("fixture component has an operation");
        component_operation.registered_operation =
            Some("overlay:widget/get@3.0.0".parse().unwrap());
        mismatched_components.insert(component);
        let _error =
            ServingManifest::new(release(), mismatched_components, routes(), BTreeMap::new())
                .expect_err("component operation must match its selected package coordinate");
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
                BTreeMap::from([("orders".into(), signed)]),
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
                    BTreeMap::from([("orders".to_string(), malformed)]),
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
                BTreeMap::from([("orders".to_string(), authenticated)]),
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
                BTreeMap::from([("orders".to_string(), anonymous)]),
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
            .insert(PackageCoordinate::new("base", "2.0.0".parse().unwrap()).unwrap());
        let error = ServingManifest::new(duplicate, components(), routes(), BTreeMap::new())
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

        let mut both = route.clone();
        both["wiring-id"] = serde_json::json!("retired");
        both["wiring-version"] = serde_json::json!(1);
        let mut neither = route.clone();
        neither.as_object_mut().unwrap().remove("component");
        neither.as_object_mut().unwrap().remove("operation");
        let mut half = route;
        half.as_object_mut().unwrap().remove("operation");
        for document in [both, neither, half] {
            let _error = serde_json::from_value::<ServingAttachment>(document)
                .expect_err("an attachment requires a route target");
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
                BTreeMap::from([("widget-get".to_string(), absent)]),
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
                BTreeMap::from([("widget-get".to_string(), other_package)]),
            ),
            Err(CatalogIdentityError::UnresolvableManifestRoute { .. })
        ));

        let mut mismatched = route_attachment();
        mismatched.registered_operation = Some("base:widget/list@1.0.0".parse().unwrap());
        let _error = ServingManifest::new(
            release(),
            components(),
            routes(),
            BTreeMap::from([("widget-get".to_string(), mismatched)]),
        )
        .expect_err("a route attachment cannot grant another operation");
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
        for routes in [
            BTreeSet::from([unexported]),
            BTreeSet::from([handler]),
            duplicate,
        ] {
            let _error = ServingManifest::new(release(), components(), routes, BTreeMap::new())
                .expect_err("an inexact route must refuse");
        }
    }
}
