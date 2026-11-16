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

    pub fn is_empty(&self) -> bool {
        true
    }
}

impl crate::Changes for AuthDrops {}
