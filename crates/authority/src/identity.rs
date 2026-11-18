//! Host-attested session identity and connection snapshots.

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
}
