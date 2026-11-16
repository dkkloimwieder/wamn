//! Host-attested session identity and connection snapshots.

use anyhow::Context as _;
use serde::Deserialize;
use std::sync::Arc;

use wamn_engine::release_manifest::LoadedRelease;

/// The release a pod carries — the manifest digest derived from the verified
/// content of its mounted serving manifest
/// ([`LoadedRelease`](wamn_engine::release_manifest::LoadedRelease)).
///
/// Admission pins the release digest. The production identity verifies that pin
/// against the binding pod's digest. The value is host-injected identity,
/// never guest-supplied.
///
/// The field is private, so production code builds the value only from a
/// loaded release (wamn-p77b).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseIdentity {
    /// The serving manifest's digest — `runs.manifest_digest`. The
    /// `sha256:<64 lowercase hex>` shape the run plane's
    /// `runs_release_record_check` admits is carried by the type, so there is no
    /// hand-rolled shape test on this path.
    manifest_digest: wamn_digest::Digest,
}

impl ReleaseIdentity {
    /// The identity of the release a pod loaded.
    pub fn from_loaded(release: &LoadedRelease) -> Self {
        Self {
            manifest_digest: release.release().manifest_digest,
        }
    }

    /// An identity with no loaded release, for tests whose rows name it.
    #[cfg(feature = "test-util")]
    pub fn for_test(manifest_digest: wamn_digest::Digest) -> Self {
        Self { manifest_digest }
    }

    /// `runs.manifest_digest`.
    pub fn manifest_digest(&self) -> &wamn_digest::Digest {
        &self.manifest_digest
    }
}

/// The complete host-injected identity set one component id resolves to.
///
/// Every field is a registry this plugin keys by component id, so this type is
/// the whole of what [`WamnPostgres::bind_session_identity`] writes and
/// [`WamnPostgres::revoke_session_identity`] clears. Adding a registry without
/// adding a field here leaves an identity that no acquisition rebinds — which is
/// exactly the cross-project_env_key leak `wamn-0h0g.17.7` closes.
///
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionIdentity {
    /// The target project environment. A session without it is refused by
    /// [`WamnPostgres::require_project_env_key`] before selecting a database.
    pub project_env_key: Option<wamn_registry::ProjectEnvKey>,
    /// Which project database this session resolves against. `None` ⇒ the
    /// default project.
    pub project: Option<wamn_registry::ProjectId>,
    /// `SET LOCAL search_path`. `None` ⇒ the server's own search_path.
    pub schema: Option<String>,
    pub runner: Option<String>,
    pub role: Option<String>,
    pub user_id: Option<wamn_identity_protocol::PrincipalId>,
    pub operation: Option<wamn_catalog::canonical_name::CanonicalName>,
    pub application: Option<wamn_catalog::ApplicationName>,
    /// The release digest the binding pod carries.
    pub release: Option<ReleaseIdentity>,
}

/// The route package or application workflow that owns an effect's entry.
#[derive(Debug, Clone, Copy)]
pub enum ConnectionEntryLookup<'a> {
    Route {
        package_id: &'a wamn_catalog::PackageName,
    },
    Wiring {
        application: &'a wamn_catalog::ApplicationName,
        wiring: WiringLookup<'a>,
    },
}

/// One node of an exact wiring version.
#[derive(Debug, Clone, Copy)]
pub struct WiringLookup<'a> {
    pub wiring_id: &'a wamn_catalog::canonical_name::CanonicalName,
    pub wiring_version: i32,
    pub node_id: &'a wamn_catalog::canonical_name::CanonicalName,
}

/// Host-only identity used to load one HTTP effect authorization snapshot.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionEffectLookup<'a> {
    pub entry: ConnectionEntryLookup<'a>,
    pub origin_package_id: &'a wamn_catalog::PackageName,
    pub origin_component_digest: &'a wamn_digest::Digest,
    pub origin_component: &'a wamn_catalog::canonical_name::CanonicalName,
    pub origin_interface_version: &'a wamn_catalog::canonical_name::CanonicalName,
    pub origin_operation: &'a wamn_catalog::canonical_name::CanonicalName,
    pub package_id: &'a wamn_catalog::PackageName,
    pub operation: &'a wamn_catalog::canonical_name::CanonicalName,
    /// The release the effect runs under, as its manifest digest.
    pub manifest_digest: &'a wamn_digest::Digest,
    pub environment: &'a wamn_registry::EnvName,
    pub component_digest: &'a wamn_digest::Digest,
    pub store_alias: &'a wamn_catalog::canonical_name::CanonicalName,
    pub candidate_binding: Option<&'a CandidateConnectionBinding>,
}

/// One DB-derived, non-secret connection fact frozen on a candidate run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CandidateConnectionBinding {
    pub read_only: bool,
    pub component_digest: wamn_digest::Digest,
    pub store_alias: wamn_catalog::canonical_name::CanonicalName,
    pub requirement_hash: wamn_digest::Digest,
    pub instance_id: String,
    pub instance_revision: i64,
    pub requirement_type: String,
    pub contract: String,
    pub validation_hash: wamn_digest::Digest,
    pub generation: i64,
    pub definition_hash: wamn_digest::Digest,
    /// None for a `gcs` blobstore, which signs with the host pod's service account.
    pub credential_set_handle: Option<String>,
}

impl CandidateConnectionBinding {
    fn is_complete(&self) -> bool {
        !self.instance_id.is_empty()
            && self.instance_revision >= 0
            && !self.requirement_type.is_empty()
            && !self.contract.is_empty()
            && self.generation > 0
            && self
                .credential_set_handle
                .as_deref()
                .is_none_or(|handle| !handle.is_empty())
    }

    pub fn matches_snapshot(&self, snapshot: &ConnectionEffectSnapshot) -> bool {
        snapshot.read_only == self.read_only
            && snapshot.requirement_hash.as_ref() == Some(&self.requirement_hash)
            && snapshot.instance_id.as_deref() == Some(self.instance_id.as_str())
            && snapshot
                .instance_revision
                .is_some_and(|revision| revision >= self.instance_revision)
            && snapshot.requirement_type.as_deref() == Some(self.requirement_type.as_str())
            && snapshot.contract.as_deref() == Some(self.contract.as_str())
            && snapshot.validation_hash.as_ref() == Some(&self.validation_hash)
            && snapshot.pinned_generation == Some(self.generation)
            && snapshot.generation == Some(self.generation)
            && snapshot.definition_hash.as_ref() == Some(&self.definition_hash)
            && snapshot.credential_handle == self.credential_set_handle
    }
}

/// Canonically ordered binding world persisted by private candidate admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateBindingWorld(Arc<[CandidateConnectionBinding]>);

impl CandidateBindingWorld {
    /// Decode the exact persisted JSON boundary and reject partial, duplicate,
    /// or non-canonical rows before any component executes.
    pub fn from_json(value: serde_json::Value) -> anyhow::Result<Self> {
        let bindings: Vec<CandidateConnectionBinding> =
            serde_json::from_value(value).context("decode candidate binding world")?;
        anyhow::ensure!(
            bindings.iter().all(CandidateConnectionBinding::is_complete),
            "candidate-binding-world-incomplete"
        );
        anyhow::ensure!(
            bindings.windows(2).all(|pair| {
                (&pair[0].component_digest, &pair[0].store_alias)
                    < (&pair[1].component_digest, &pair[1].store_alias)
            }),
            "candidate-binding-world-not-canonical"
        );
        Ok(Self(bindings.into()))
    }

    /// Find the one frozen binding selected by a component store alias.
    pub fn binding(
        &self,
        component_digest: &wamn_digest::Digest,
        store_alias: &wamn_catalog::canonical_name::CanonicalName,
    ) -> Option<&CandidateConnectionBinding> {
        self.0.iter().find(|binding| {
            binding.component_digest == *component_digest && &binding.store_alias == store_alias
        })
    }
}

/// Resolved invocation authority and current connection facts before an effect.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the booleans record independent invocation, binding and connection checks"
)]
pub struct ConnectionEffectSnapshot {
    pub connection_name: Option<String>,
    pub read_only: bool,
    /// `None` for a route, which has no wiring.
    pub wiring_hash: Option<wamn_digest::Digest>,
    pub component: Option<wamn_catalog::canonical_name::CanonicalName>,
    pub interface_version: Option<wamn_catalog::canonical_name::CanonicalName>,
    pub operation: Option<wamn_catalog::canonical_name::CanonicalName>,
    pub registered_operation: Option<wamn_catalog::registered_operation::RegisteredOperation>,
    pub requirement_json: Option<serde_json::Value>,
    pub requirement_hash: Option<wamn_digest::Digest>,
    pub node_permitted: bool,
    pub binding_active: bool,
    pub binding_valid: bool,
    pub instance_id: Option<String>,
    pub validation_hash: Option<wamn_digest::Digest>,
    pub requirement_type: Option<String>,
    pub contract: Option<String>,
    pub instance_enabled: bool,
    pub active_generation: Option<i64>,
    /// The generation frozen by admission, if this invocation carries a pin.
    pub pinned_generation: Option<i64>,
    pub instance_revision: Option<i64>,
    pub generation: Option<i64>,
    pub definition: Option<serde_json::Value>,
    pub definition_hash: Option<wamn_digest::Digest>,
    pub credential_handle: Option<String>,
}
