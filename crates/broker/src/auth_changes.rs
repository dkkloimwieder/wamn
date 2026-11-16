/// One auth fact that a committed write changed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuthChange {
    /// The user row or the user roles of `principal` in `project_env_key`.
    Principal {
        project_env_key: wamn_registry::ProjectEnvKey,
        principal: wamn_identity_protocol::PrincipalId,
    },
    /// A role or a permission of `project_env_key`, which every principal holding the
    /// role reads, or the environment status of `project_env_key`.
    ProjectEnvKey(wamn_registry::ProjectEnvKey),
    /// A PAT, a login, a membership or the status of `principal`. These are
    /// facts of the organization, so they change the principal in every
    /// project.
    Identity(wamn_identity_protocol::PrincipalId),
}

/// The changes of one transaction, published after its commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthDrops {}

impl AuthDrops {
    pub fn principal(
        &mut self,
        _project_env_key: &wamn_registry::ProjectEnvKey,
        _principal: &wamn_identity_protocol::PrincipalId,
    ) {
    }

    pub fn project_env_key(&mut self, _project_env_key: &wamn_registry::ProjectEnvKey) {}

    pub fn identity(&mut self, _principal: &wamn_identity_protocol::PrincipalId) {}

    /// Add the changes of `other`, for a caller that runs several writers in
    /// one transaction.
    pub fn extend(&mut self, _other: AuthDrops) {}

    pub fn changes(&self) -> impl Iterator<Item = &AuthChange> {
        std::iter::empty()
    }

    pub fn is_empty(&self) -> bool {
        true
    }
}

/// The NATS KV bucket of the auth memory, on the scheduler NATS.
pub const BUCKET: &str = "wamn-auth";

/// Open the bucket. Identity makes it ([`crate::kv_memory::create_buckets`]).
pub async fn auth_bucket(
    client: &async_nats::Client,
) -> anyhow::Result<async_nats::jetstream::kv::Store> {
    crate::kv_memory::open_bucket(client, BUCKET).await
}

impl crate::Changes for AuthDrops {}
