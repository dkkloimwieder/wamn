//! Canonical immutable catalog identities and definition hashing.
//!
//! MVP outcome: provisioning · publish · additive schema · tenant isolation (T1 minting).
//!
//! This crate owns the pure definition plane, and — since wamn-0h0g.18.2 — the
//! statement text of the wiring-activation verb over it. The pointer's write and
//! its env-hot read are one contract confirming one definition hash, and this is
//! the only crate the management driver and the serving plane both already
//! depend on. Persistence, publication, activation transitions, and
//! compatibility readers still live in effect crates: no connection,
//! transaction, or clock is reachable from here.

mod component_library;
mod connection;
mod package;
mod serving_manifest;
mod wiring;
mod wiring_activation;
mod wiring_compatibility;

pub use component_library::{
    AdmittedComponent, AdmittedComponentEffect, AdmittedComponentFacts, AdmittedComponentOperation,
    AdmittedComponentParameter, AdmittedComponentPort, ComponentConnection,
    ComponentConnectionType, ComponentDeclaration, ComponentFactError, ComponentFactErrorKind,
    ComponentOperationDeclaration, ComponentOperationDependency, ComponentPackageScope,
    ComponentParameterDeclaration, ComponentPortDeclaration, ComponentSchema, ComponentSqlField,
    ComponentSqlStatement, ComponentSqlValueType, bind_component_statement_facts,
    component_sql_digest, normalize_component_fact, schema_digests_match,
    verify_stored_effect_projection,
};
pub use connection::{
    CONNECTION_DESCRIPTOR_VERSION, ConnectionAuthorityModel, ConnectionField, ConnectionFieldOwner,
    ConnectionFieldOwnership, ConnectionTypeDescriptor, CredentialInjection,
};
pub use package::{EffectiveReleaseId, PackageCoordinate};
pub use serving_manifest::{
    INVALID_ATTACHMENT_AUTH_POLICY_REFUSAL, MAX_SERVING_MANIFEST_BYTES, NO_AUTHENTICATION_MODE,
    PAT_AUTHENTICATION_MODE, RELEASE_MANIFEST_CONFIGMAP_PREFIX, RELEASE_MANIFEST_FILE_NAME,
    RELEASE_MANIFEST_MOUNT_PATH, SERVING_MANIFEST_FORMAT_VERSION, ServingAttachment,
    ServingComponent, ServingComponentOperation, ServingManifest, ServingRegistration,
    ServingRegistrationInput, ServingRelease, ServingWiring,
    UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL, release_manifest_configmap_name,
};
pub use wiring::{
    WIRING_DOCUMENT_FORMAT_VERSION, WiringDocument, WiringEdge, WiringEventOperation, WiringNode,
    WiringOperationDependency, WiringTerminal,
};
pub use wiring_activation::{
    WIRING_ACTIVATION_CHANNEL, WiringActivationNotice, flip_activation,
    previous_confirmed_definition, record_activation_event, resolve_active_wiring,
};
pub use wiring_compatibility::{
    WiringCompatibilityError, WiringCompatibilityErrorKind, validate_resolved_wiring_compatibility,
    validate_wiring_compatibility,
};

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

const HASH_PREFIX: &str = "sha256:";
const HASH_HEX_LEN: usize = 64;
const IDENTITY_FORMAT: &[u8] = b"wamn.catalog.identity.v0.1";

/// A catalog identity construction error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogIdentityError {
    EmptyIdentity {
        field: &'static str,
    },
    NonCanonicalIdentity {
        field: &'static str,
    },
    ZeroVersion {
        field: &'static str,
    },
    InvalidDigest {
        field: &'static str,
    },
    InvalidDefinition {
        message: String,
    },
    NonCanonicalJson,
    MutableIdentityInput {
        field: String,
    },
    GraphHashMismatch,
    ArtifactIdMismatch,
    ArtifactHashMismatch,
    FlowInvalid {
        codes: Vec<&'static str>,
    },
    DuplicateMember {
        field: &'static str,
        id: String,
    },
    NonCanonicalMemberOrder {
        field: &'static str,
        id: String,
    },
    ArtifactMismatch,
    UnresolvedSource {
        source_id: String,
    },
    SourceMismatch {
        source_id: String,
    },
    UnsupportedServingManifestVersion {
        requested: String,
    },
    InvalidAttachmentAuthPolicy {
        attachment_id: String,
    },
    UnauthenticatedRegisteredOperation {
        attachment_id: String,
    },
    UnresolvableManifestWiring {
        package_id: String,
        wiring_id: String,
        wiring_version: u32,
    },
    ManifestTooLarge {
        bytes: usize,
        limit: usize,
    },
    UnresolvedWiringNode {
        node_id: String,
    },
    UnresolvedWiringEntry {
        node_id: String,
    },
}

impl fmt::Display for CatalogIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyIdentity { field } => write!(formatter, "{field} is empty"),
            Self::NonCanonicalIdentity { field } => {
                write!(formatter, "{field} is not in canonical form")
            }
            Self::ZeroVersion { field } => write!(formatter, "{field} must be greater than zero"),
            Self::InvalidDigest { field } => {
                write!(
                    formatter,
                    "{field} must be sha256:<64 lowercase hex digits>"
                )
            }
            Self::InvalidDefinition { message } => write!(formatter, "{message}"),
            Self::NonCanonicalJson => write!(formatter, "JSON input is not RFC 8785 canonical"),
            Self::MutableIdentityInput { field } => {
                write!(
                    formatter,
                    "mutable field {field:?} cannot enter definition identity"
                )
            }
            Self::GraphHashMismatch => write!(formatter, "flow graph hash does not match"),
            Self::ArtifactIdMismatch => {
                write!(
                    formatter,
                    "flow graph does not match the pinned artifact id"
                )
            }
            Self::ArtifactHashMismatch => write!(formatter, "flow artifact hash does not match"),
            Self::FlowInvalid { codes } => {
                write!(formatter, "flow validation failed: {}", codes.join(", "))
            }
            Self::DuplicateMember { field, id } => {
                write!(formatter, "{field} member {id:?} is duplicated")
            }
            Self::NonCanonicalMemberOrder { field, id } => {
                write!(formatter, "{field} member {id:?} is not in canonical order")
            }
            Self::ArtifactMismatch => write!(formatter, "attachment artifact does not match"),
            Self::UnresolvedSource { source_id } => {
                write!(formatter, "source {source_id:?} is unresolved")
            }
            Self::SourceMismatch { source_id } => {
                write!(
                    formatter,
                    "source {source_id:?} differs from its resolved definition"
                )
            }
            Self::UnsupportedServingManifestVersion { requested } => {
                write!(
                    formatter,
                    "{}: requested {requested}; supported version is {}",
                    UNSUPPORTED_SERVING_MANIFEST_VERSION_REFUSAL, SERVING_MANIFEST_FORMAT_VERSION
                )
            }
            Self::InvalidAttachmentAuthPolicy { attachment_id } => {
                write!(
                    formatter,
                    "{INVALID_ATTACHMENT_AUTH_POLICY_REFUSAL}: attachment {attachment_id:?} must declare exactly one supported mode"
                )
            }
            Self::UnauthenticatedRegisteredOperation { attachment_id } => {
                write!(
                    formatter,
                    "attachment {attachment_id:?} cannot combine auth-policy.mode=\"none\" with registered-operation; set auth-policy.mode=\"pat\""
                )
            }
            Self::UnresolvableManifestWiring {
                package_id,
                wiring_id,
                wiring_version,
            } => {
                write!(
                    formatter,
                    "wiring {package_id:?}/{wiring_id:?} version {wiring_version} is absent from the serving manifest"
                )
            }
            Self::ManifestTooLarge { bytes, limit } => {
                write!(
                    formatter,
                    "serving manifest is {bytes} bytes, over the {limit}-byte delivery limit"
                )
            }
            Self::UnresolvedWiringNode { node_id } => {
                write!(
                    formatter,
                    "wiring edge names node {node_id:?}, which the document does not declare"
                )
            }
            Self::UnresolvedWiringEntry { node_id } => {
                write!(
                    formatter,
                    "wiring entry names node {node_id:?}, which the document does not declare"
                )
            }
        }
    }
}

impl std::error::Error for CatalogIdentityError {}

fn validate_text(value: &str, field: &'static str) -> Result<(), CatalogIdentityError> {
    if value.is_empty() {
        return Err(CatalogIdentityError::EmptyIdentity { field });
    }
    if value.trim() != value || value.as_bytes().contains(&0) {
        return Err(CatalogIdentityError::NonCanonicalIdentity { field });
    }
    Ok(())
}

/// A canonical SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct DefinitionHash(String);

impl DefinitionHash {
    /// Parse a canonical SHA-256 definition hash.
    pub fn parse(value: impl Into<String>) -> Result<Self, CatalogIdentityError> {
        let value = value.into();
        validate_digest(&value, "definition-hash")?;
        Ok(Self(value))
    }

    /// The `sha256:<hex>` representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for DefinitionHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for DefinitionHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A canonical artifact-content digest.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ArtifactHash(String);

impl ArtifactHash {
    /// Parse a canonical SHA-256 artifact-content digest.
    pub fn parse(value: impl Into<String>) -> Result<Self, CatalogIdentityError> {
        let value = value.into();
        validate_digest(&value, "artifact-hash")?;
        Ok(Self(value))
    }

    /// The `sha256:<hex>` representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ArtifactHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for ArtifactHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The digest of a serving manifest's RFC 8785 canonical bytes.
///
/// This is the one *derived* identity in the family: it is never asserted by a
/// carrier and never read from an object name, only computed from verified
/// content by [`ServingManifest::from_canonical_bytes`]. From there it travels
/// further than any other hash in the system — through the claim-time run
/// recording, effect-authority equality, and the deployment attestation, across
/// two host processes and four manifest readers. Its distinct type keeps it
/// apart from the manifest's [`ArtifactHash`] and [`DefinitionHash`] members.
///
/// Unlike the manifest-carried [`DefinitionHash`] and [`ArtifactHash`], it has
/// no `Deserialize`: a carrier must not assert this derived identity. The only
/// admitting boundary computes it from verified canonical bytes through
/// [`ServingManifest::from_canonical_bytes`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ManifestDigest(String);

impl ManifestDigest {
    /// Parse a canonical SHA-256 serving-manifest digest.
    pub fn parse(value: impl Into<String>) -> Result<Self, CatalogIdentityError> {
        let value = value.into();
        validate_digest(&value, "manifest-digest")?;
        Ok(Self(value))
    }

    /// The `sha256:<hex>` representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ManifestDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn validate_digest(value: &str, field: &'static str) -> Result<(), CatalogIdentityError> {
    let valid = value.strip_prefix(HASH_PREFIX).is_some_and(|hex| {
        hex.len() == HASH_HEX_LEN
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if valid {
        Ok(())
    } else {
        Err(CatalogIdentityError::InvalidDigest { field })
    }
}

fn digest(input: &[u8]) -> String {
    let digest = Sha256::digest(input);
    let mut output = String::with_capacity(HASH_PREFIX.len() + HASH_HEX_LEN);
    output.push_str(HASH_PREFIX);
    for byte in digest {
        use fmt::Write as _;
        write!(&mut output, "{byte:02x}").expect("writing to a string is infallible");
    }
    output
}

/// The tenant-scoped immutable identity of a flow artifact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ArtifactId {
    tenant_id: String,
    flow_id: String,
    flow_version: u32,
}

impl ArtifactId {
    pub fn new(
        tenant_id: impl Into<String>,
        flow_id: impl Into<String>,
        flow_version: u32,
    ) -> Result<Self, CatalogIdentityError> {
        let tenant_id = tenant_id.into();
        let flow_id = flow_id.into();
        validate_text(&tenant_id, "tenant-id")?;
        validate_text(&flow_id, "flow-id")?;
        if flow_version == 0 {
            return Err(CatalogIdentityError::ZeroVersion {
                field: "flow-version",
            });
        }
        Ok(Self {
            tenant_id,
            flow_id,
            flow_version,
        })
    }

    pub fn tenant_id(&self) -> &str {
        &self.tenant_id
    }

    pub fn flow_id(&self) -> &str {
        &self.flow_id
    }

    pub fn flow_version(&self) -> u32 {
        self.flow_version
    }
}

macro_rules! string_id {
    ($name:ident, $field:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, CatalogIdentityError> {
                let value = value.into();
                validate_text(&value, $field)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

string_id!(SourceId, "source-id");
string_id!(AttachmentId, "attachment-id");

/// Canonical JSON used by immutable source and attachment definitions.
#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalJson {
    value: Value,
    bytes: Box<[u8]>,
}

impl CanonicalJson {
    /// Canonicalize a JSON object, refusing operationally mutable top-level fields.
    pub fn new(value: Value) -> Result<Self, CatalogIdentityError> {
        let object = value
            .as_object()
            .ok_or_else(|| CatalogIdentityError::InvalidDefinition {
                message: "a source or attachment definition must be a JSON object".to_string(),
            })?;
        for field in [
            "active",
            "applied-version",
            "changed-at",
            "confirmed-definition-hash",
            "created-at",
            "enabled",
            "updated-at",
        ] {
            if object.contains_key(field) {
                return Err(CatalogIdentityError::MutableIdentityInput {
                    field: field.to_string(),
                });
            }
        }
        let bytes = wamn_execution_contract::canonical_json_bytes(&value).into_boxed_slice();
        Ok(Self { value, bytes })
    }

    /// Parse bytes that must already be in RFC 8785 canonical form.
    pub fn parse(input: &str) -> Result<Self, CatalogIdentityError> {
        let value: Value = serde_json::from_str(input).map_err(|error| {
            CatalogIdentityError::InvalidDefinition {
                message: format!("definition JSON is invalid: {error}"),
            }
        })?;
        let canonical = Self::new(value)?;
        if canonical.bytes.as_ref() != input.as_bytes() {
            return Err(CatalogIdentityError::NonCanonicalJson);
        }
        Ok(canonical)
    }

    pub fn as_value(&self) -> &Value {
        &self.value
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// The complete immutable artifact identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ArtifactIdentity {
    id: ArtifactId,
    artifact_hash: ArtifactHash,
}

impl ArtifactIdentity {
    /// Pair a parsed artifact id with the artifact-content digest recorded
    /// beside it.
    ///
    /// Both halves are already-validated newtypes, so this only joins them. It
    /// replaced `Artifact::new`, which derived the same pair by hashing a
    /// parsed flow graph — the flow language retired in wamn-0h0g.26.5 and the
    /// artifact bytes it hashed are no longer produced here.
    pub fn new(id: ArtifactId, artifact_hash: ArtifactHash) -> Self {
        Self { id, artifact_hash }
    }

    pub fn id(&self) -> &ArtifactId {
        &self.id
    }

    pub fn artifact_hash(&self) -> &ArtifactHash {
        &self.artifact_hash
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        frames(
            "artifact-identity",
            [
                ("tenant-id", self.id.tenant_id.as_bytes()),
                ("flow-id", self.id.flow_id.as_bytes()),
                ("flow-version", &self.id.flow_version.to_be_bytes()),
                ("artifact-hash", self.artifact_hash.as_str().as_bytes()),
            ],
        )
    }
}

/// A source definition kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind {
    Auth,
    CallerPolicy,
    Schedule,
}

/// A canonical immutable source definition.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    id: SourceId,
    kind: SourceKind,
    definition: CanonicalJson,
    canonical_bytes: Box<[u8]>,
}

impl Source {
    pub fn new(id: SourceId, kind: SourceKind, definition: CanonicalJson) -> Self {
        let kind_bytes = serde_json::to_vec(&kind).expect("source kind serializes");
        let canonical_bytes = frames(
            "source",
            [
                ("source-id", id.as_str().as_bytes()),
                ("kind", kind_bytes.as_slice()),
                ("definition", definition.as_bytes()),
            ],
        )
        .into_boxed_slice();
        Self {
            id,
            kind,
            definition,
            canonical_bytes,
        }
    }

    pub fn id(&self) -> &SourceId {
        &self.id
    }

    pub fn kind(&self) -> SourceKind {
        self.kind
    }

    pub fn definition(&self) -> &CanonicalJson {
        &self.definition
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
}

/// An attachment exposure kind.
///
/// `Deserialize` is derived so the frozen [`ServingAttachment`] wire shape can
/// name this kind instead of re-declaring one; the enum is fieldless, so the
/// derive adds no unvalidated construction path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttachmentKind {
    Http,
    Internal,
    Studio,
    Cron,
}

/// Unresolved attachment input. Resolution is mandatory before hashing.
#[derive(Debug, Clone, PartialEq)]
pub struct AttachmentDraft {
    pub id: AttachmentId,
    pub kind: AttachmentKind,
    pub artifact_id: ArtifactId,
    pub source_ids: Vec<SourceId>,
    pub definition: CanonicalJson,
}

/// A canonical attachment definition with resolved sources.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    id: AttachmentId,
    kind: AttachmentKind,
    artifact: ArtifactIdentity,
    source_ids: Vec<SourceId>,
    resolved_sources: Vec<Source>,
    definition: CanonicalJson,
    definition_hash: DefinitionHash,
    canonical_bytes: Box<[u8]>,
}

impl Attachment {
    /// Resolve every source and compute the complete effective-contract hash.
    pub fn resolve(
        draft: AttachmentDraft,
        artifact: &ArtifactIdentity,
        sources: &[Source],
    ) -> Result<Self, CatalogIdentityError> {
        if draft.artifact_id != *artifact.id() {
            return Err(CatalogIdentityError::ArtifactMismatch);
        }
        validate_sorted_unique(&draft.source_ids, "source-ids", |id| {
            id.as_str().to_string()
        })?;
        let available: BTreeMap<_, _> = sources
            .iter()
            .map(|source| (source.id().clone(), source))
            .collect();
        let resolved: Vec<_> = draft
            .source_ids
            .iter()
            .map(|id| {
                available
                    .get(id)
                    .copied()
                    .ok_or_else(|| CatalogIdentityError::UnresolvedSource {
                        source_id: id.as_str().to_string(),
                    })
            })
            .collect::<Result<_, _>>()?;

        let kind_bytes = serde_json::to_vec(&draft.kind).expect("attachment kind serializes");
        let mut owned = vec![
            ("attachment-id", draft.id.as_str().as_bytes().to_vec()),
            ("kind", kind_bytes),
            ("artifact", artifact.canonical_bytes()),
            ("definition", draft.definition.as_bytes().to_vec()),
        ];
        for source in &resolved {
            owned.push(("resolved-source", source.canonical_bytes().to_vec()));
        }
        let borrowed: Vec<_> = owned
            .iter()
            .map(|(tag, bytes)| (*tag, bytes.as_slice()))
            .collect();
        let canonical_bytes = frames("attachment-definition", borrowed).into_boxed_slice();
        let definition_hash = DefinitionHash(digest(&canonical_bytes));
        Ok(Self {
            id: draft.id,
            kind: draft.kind,
            artifact: artifact.clone(),
            source_ids: draft.source_ids,
            resolved_sources: resolved.into_iter().cloned().collect(),
            definition: draft.definition,
            definition_hash,
            canonical_bytes,
        })
    }

    pub fn id(&self) -> &AttachmentId {
        &self.id
    }

    pub fn kind(&self) -> AttachmentKind {
        self.kind
    }

    pub fn artifact(&self) -> &ArtifactIdentity {
        &self.artifact
    }

    pub fn source_ids(&self) -> &[SourceId] {
        &self.source_ids
    }

    pub fn definition(&self) -> &CanonicalJson {
        &self.definition
    }

    pub fn definition_hash(&self) -> &DefinitionHash {
        &self.definition_hash
    }

    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }
}

fn validate_sorted_unique<T>(
    values: &[T],
    field: &'static str,
    key: impl Fn(&T) -> String,
) -> Result<(), CatalogIdentityError> {
    let mut previous: Option<String> = None;
    for value in values {
        let current = key(value);
        if let Some(previous) = &previous {
            match previous.cmp(&current) {
                Ordering::Equal => {
                    return Err(CatalogIdentityError::DuplicateMember { field, id: current });
                }
                Ordering::Greater => {
                    return Err(CatalogIdentityError::NonCanonicalMemberOrder {
                        field,
                        id: current,
                    });
                }
                Ordering::Less => {}
            }
        }
        previous = Some(current);
    }
    Ok(())
}

fn frames<'a>(domain: &str, values: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> Vec<u8> {
    frames_with_format(IDENTITY_FORMAT, domain, values)
}

fn frames_with_format<'a>(
    format: &[u8],
    domain: &str,
    values: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Vec<u8> {
    let mut output = Vec::new();
    write_frame(&mut output, format);
    write_frame(&mut output, domain.as_bytes());
    for (tag, value) in values {
        write_frame(&mut output, tag.as_bytes());
        write_frame(&mut output, value);
    }
    output
}

fn write_frame(output: &mut Vec<u8>, value: &[u8]) {
    let length = u64::try_from(value.len()).expect("identity field length fits u64");
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(value);
}

/// Canonical identity-frame bytes for one serializable identity input.
///
/// Routes through `wamn_execution_contract::canonical_json_bytes`, the workspace's only RFC
/// 8785 producer. Until wamn-0h0g.15.63 this crate carried a second, `ryu-js`
/// based implementation of the same spec beside it, which left release identity
/// depending on *which* producer a call site happened to reach for.
fn canonical_serialized(value: &impl Serialize) -> Vec<u8> {
    wamn_execution_contract::canonical_json_bytes(
        &serde_json::to_value(value).expect("identity input serializes"),
    )
}

#[cfg(test)]
mod tests {
    use super::{ManifestDigest, digest, frames};

    /// The invariant that retired `valid_manifest_digest` in the claim path: the
    /// newtype admits exactly the `sha256:<64 lowercase hex>` shape the run
    /// plane's `runs_release_record_check` admits, so a parsed value can never
    /// die on that CHECK inside a lease grant.
    #[test]
    fn a_manifest_digest_admits_exactly_the_run_plane_shape() {
        assert!(ManifestDigest::parse(format!("sha256:{}", "0".repeat(64))).is_ok());
        assert!(ManifestDigest::parse(format!("sha256:{}b", "af9".repeat(21))).is_ok());
        for rejected in [
            String::new(),
            "sha256:".to_string(),
            "deadbeef".to_string(),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(65)),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha256:{}", "g".repeat(64)),
            format!("SHA256:{}", "a".repeat(64)),
        ] {
            assert!(
                ManifestDigest::parse(rejected.clone()).is_err(),
                "accepted {rejected:?}"
            );
        }
    }

    #[test]
    fn named_removal_and_reordering_mutants_change_every_hash_frame() {
        for (domain, fields) in [
            (
                "artifact",
                vec![
                    ("artifact-id", b"id".as_slice()),
                    ("schema-version", b"schema".as_slice()),
                    ("graph", b"graph".as_slice()),
                    ("interface", b"interface".as_slice()),
                    ("supplied-component", b"component".as_slice()),
                ],
            ),
            (
                "attachment-definition",
                vec![
                    ("attachment-id", b"id".as_slice()),
                    ("kind", b"kind".as_slice()),
                    ("artifact", b"artifact".as_slice()),
                    ("definition", b"definition".as_slice()),
                    ("resolved-source", b"source".as_slice()),
                ],
            ),
        ] {
            let baseline = digest(&frames(domain, fields.iter().copied()));
            for (index, (tag, _)) in fields.iter().enumerate() {
                let removal: Vec<_> = fields
                    .iter()
                    .enumerate()
                    .filter(|(candidate, _)| *candidate != index)
                    .map(|(_, field)| *field)
                    .collect();
                assert_ne!(
                    baseline,
                    digest(&frames(domain, removal)),
                    "named removal mutant {domain}.{tag} survived"
                );

                let mut reordering = fields.clone();
                let field = reordering.remove(index);
                let destination = if index + 1 == fields.len() {
                    0
                } else {
                    index + 1
                };
                reordering.insert(destination, field);
                assert_ne!(
                    baseline,
                    digest(&frames(domain, reordering)),
                    "named reordering mutant {domain}.{tag} survived"
                );
            }
        }
    }
}
