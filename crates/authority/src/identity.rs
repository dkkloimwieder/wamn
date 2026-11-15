//! Host-attested session identity and connection snapshots.

use anyhow::Context as _;
use serde::Deserialize;
use std::sync::Arc;
use wamn_catalog::ManifestDigest;
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
    manifest_digest: ManifestDigest,
}

impl ReleaseIdentity {
    /// The identity of the release a pod loaded.
    pub fn from_loaded(release: &LoadedRelease) -> Self {
        Self {
            manifest_digest: release.release().manifest_digest.clone(),
        }
    }

    /// An identity with no loaded release, for tests whose rows name it.
    #[cfg(feature = "test-util")]
    pub fn for_test(manifest_digest: ManifestDigest) -> Self {
        Self { manifest_digest }
    }

    /// `runs.manifest_digest`.
    pub fn manifest_digest(&self) -> &ManifestDigest {
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
/// `None` means *no identity*, not *keep the previous one*: the base denial policys
/// (`app.role` = `''`, `app.user_id` = NULL, no `search_path` override) are what
/// an acquisition that declares nothing must get.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionIdentity {
    /// `app.project_env_key` — the RLS identity. Required: a session with no project_env_key is
    /// refused by [`WamnPostgres::require_project_env_key`] rather than run without an identity.
    pub project_env_key: String,
    /// Which project database this session resolves against. `None` ⇒ the
    /// default project.
    pub project: Option<String>,
    /// `SET LOCAL search_path`. `None` ⇒ the server's own search_path.
    pub schema: Option<String>,
    /// `app.runner` — the durable-queue lease owner.
    pub runner: Option<String>,
    /// `app.role` — the caller's `roles.name` for compiled per-role RLS.
    pub role: Option<String>,
    /// `app.user_id` — the executing principal's `users.id`, which the
    /// record-history triggers stamp. A transactional statement with none is
    /// refused.
    pub user_id: Option<String>,
    /// `app.operation`: the executing operation token, or `wamn:<component>`
    /// for a platform component outside a registered operation.
    pub operation: Option<String>,
    /// `app.application`: the environment map name of the application that
    /// holds the operation's package.
    pub application: Option<String>,
    /// The release digest the binding pod carries.
    pub release: Option<ReleaseIdentity>,
}

/// The route or wiring position one effect snapshot resolves under.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionEntryLookup<'a> {
    /// The package that owns the route or the wiring.
    pub package_id: &'a str,
    /// `None` for a route, which has no wiring position.
    pub wiring: Option<WiringLookup<'a>>,
}

/// One node of an exact wiring version.
#[derive(Debug, Clone, Copy)]
pub struct WiringLookup<'a> {
    pub wiring_id: &'a str,
    pub wiring_version: i32,
    pub node_id: &'a str,
}

/// Host-only identity used to load one HTTP effect authorization snapshot.
#[derive(Debug, Clone, Copy)]
pub struct ConnectionEffectLookup<'a> {
    pub entry: ConnectionEntryLookup<'a>,
    pub origin_package_id: &'a str,
    pub origin_component_digest: &'a str,
    pub origin_component: &'a str,
    pub origin_interface_version: &'a str,
    pub origin_operation: &'a str,
    pub package_id: &'a str,
    pub operation: &'a str,
    /// The release the effect runs under, as its manifest digest.
    pub manifest_digest: &'a str,
    pub environment: &'a str,
    pub component_digest: &'a str,
    pub store_alias: &'a str,
    pub candidate_binding: Option<&'a CandidateConnectionBinding>,
}

/// One DB-derived, non-secret connection fact frozen on a candidate run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CandidateConnectionBinding {
    pub read_only: bool,
    pub component_digest: String,
    pub store_alias: String,
    pub requirement_hash: String,
    pub instance_id: String,
    pub instance_revision: i64,
    pub requirement_type: String,
    pub contract: String,
    pub validation_hash: String,
    pub generation: i64,
    pub definition_hash: String,
    /// None for a `gcs` blobstore, which signs with the host pod's service account.
    pub credential_set_handle: Option<String>,
}

impl CandidateConnectionBinding {
    fn is_complete(&self) -> bool {
        !self.component_digest.is_empty()
            && !self.store_alias.is_empty()
            && !self.requirement_hash.is_empty()
            && !self.instance_id.is_empty()
            && self.instance_revision >= 0
            && !self.requirement_type.is_empty()
            && !self.contract.is_empty()
            && !self.validation_hash.is_empty()
            && self.generation > 0
            && !self.definition_hash.is_empty()
            && self
                .credential_set_handle
                .as_deref()
                .is_none_or(|handle| !handle.is_empty())
    }

    pub fn matches_snapshot(&self, snapshot: &ConnectionEffectSnapshot) -> bool {
        snapshot.read_only == self.read_only
            && snapshot.requirement_hash.as_deref() == Some(self.requirement_hash.as_str())
            && snapshot.instance_id.as_deref() == Some(self.instance_id.as_str())
            && snapshot
                .instance_revision
                .is_some_and(|revision| revision >= self.instance_revision)
            && snapshot.requirement_type.as_deref() == Some(self.requirement_type.as_str())
            && snapshot.contract.as_deref() == Some(self.contract.as_str())
            && snapshot.validation_hash.as_deref() == Some(self.validation_hash.as_str())
            && snapshot.pinned_generation == Some(self.generation)
            && snapshot.generation == Some(self.generation)
            && snapshot.definition_hash.as_deref() == Some(self.definition_hash.as_str())
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
        component_digest: &str,
        store_alias: &str,
    ) -> Option<&CandidateConnectionBinding> {
        self.0.iter().find(|binding| {
            binding.component_digest == component_digest && binding.store_alias == store_alias
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
    pub wiring_hash: Option<String>,
    pub component: Option<String>,
    pub interface_version: Option<String>,
    pub operation: Option<String>,
    pub registered_operation: Option<String>,
    pub requirement_json: Option<serde_json::Value>,
    pub requirement_hash: Option<String>,
    pub node_permitted: bool,
    pub binding_active: bool,
    pub binding_valid: bool,
    pub instance_id: Option<String>,
    pub validation_hash: Option<String>,
    pub requirement_type: Option<String>,
    pub contract: Option<String>,
    pub instance_enabled: bool,
    pub active_generation: Option<i64>,
    /// The generation frozen by admission, if this invocation carries a pin.
    pub pinned_generation: Option<i64>,
    pub instance_revision: Option<i64>,
    pub generation: Option<i64>,
    pub definition: Option<serde_json::Value>,
    pub definition_hash: Option<String>,
    pub credential_handle: Option<String>,
}
