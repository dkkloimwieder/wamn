//! Host-attested operation session identity.

/// The complete host-injected identity set for an operation session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionIdentity {
    /// The target project environment, required before database selection.
    pub project_env_key: Option<wamn_registry::ProjectEnvKey>,
    /// Which project database this session resolves against. `None` ⇒ the
    /// default project.
    pub project: Option<wamn_registry::ProjectId>,
    /// `SET LOCAL search_path`. `None` ⇒ the server's own search_path.
    pub schema: Option<String>,
    pub runner: Option<String>,
    pub role: Option<String>,
    pub user_id: Option<wamn_identity_protocol::PrincipalId>,
    pub operation: Option<wamn_package::canonical_name::CanonicalName>,
    pub application: Option<wamn_registry::ApplicationName>,
}
