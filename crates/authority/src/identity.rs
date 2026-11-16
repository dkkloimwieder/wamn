//! Host-attested session identity and connection snapshots.

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
