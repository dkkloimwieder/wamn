//! Session ownership for environment lifecycle operations and application rollouts.

use crate::{ApplicationName, EnvName, OrgId, Triple};

/// The refusal when another operation holds the lock.
pub const DEPLOYMENT_IN_PROGRESS: &str = "deployment in progress";

/// Acquire a lifecycle lock on a session supplied by the caller.
pub trait LifecycleLocks: Sized {
    type Session;

    /// Take exclusive ownership without waiting through an active deployment.
    fn acquire(
        session: Self::Session,
        triple: &Triple,
    ) -> impl Future<Output = anyhow::Result<Self>> + Send;

    /// Take exclusive ownership of the shared environment.
    fn acquire_environment(
        session: Self::Session,
        org: &OrgId,
        env: &EnvName,
    ) -> impl Future<Output = anyhow::Result<Self>> + Send;

    /// Wait for active deployments before taking exclusive environment ownership.
    fn acquire_wait(
        session: Self::Session,
        triple: &Triple,
    ) -> impl Future<Output = anyhow::Result<Self>> + Send;

    /// Exclude database replacement while allowing independent applications to deploy.
    fn acquire_application(
        session: Self::Session,
        triple: &Triple,
        application: &ApplicationName,
    ) -> impl Future<Output = anyhow::Result<Self>> + Send;

    /// Exclude database replacement while allowing other shared owners.
    fn acquire_shared(
        session: Self::Session,
        triple: &Triple,
    ) -> impl Future<Output = anyhow::Result<Self>> + Send;
}
