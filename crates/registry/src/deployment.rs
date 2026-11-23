//! Typed reads and writes of environment placement and project registration.

use crate::{Triple, instance_suffix::InstanceSuffix};

/// Values recorded when provisioning a project deployment.
#[derive(Debug)]
pub struct Registration<'a> {
    pub triple: &'a Triple,
    pub secret_name: &'a str,
    pub secret_namespace: Option<&'a str>,
    pub instance_suffix: &'a InstanceSuffix,
    pub disposable: Option<bool>,
    pub readiness_budget_seconds: i32,
    pub drain_bound_seconds: i32,
    pub target_id: &'a str,
}

/// Environment placement and project registration.
pub trait DeploymentStore {
    type Error;

    /// Read the stored instance suffix, if the project deployment exists.
    ///
    /// # Errors
    /// Returns database errors.
    fn instance_suffix(
        &mut self,
        triple: &Triple,
    ) -> impl std::future::Future<Output = Result<Option<InstanceSuffix>, Self::Error>> + Send;

    /// Read the environment's host drain bound in seconds.
    ///
    /// # Errors
    /// Returns database errors.
    fn drain_bound_seconds(
        &mut self,
        org: &crate::OrgId,
        env: &crate::EnvName,
    ) -> impl std::future::Future<Output = Result<i32, Self::Error>> + Send;

    /// Read the projects deployed in an environment, ordered by project name.
    ///
    /// # Errors
    /// Returns database errors.
    fn projects(
        &mut self,
        org: &crate::OrgId,
        env: &crate::EnvName,
    ) -> impl std::future::Future<Output = Result<Vec<crate::ProjectId>, Self::Error>> + Send;

    /// Read whether a project deployment's environment permits resetting its database.
    ///
    /// # Errors
    /// Returns database errors.
    fn disposable(
        &mut self,
        triple: &Triple,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send;

    /// Read whether the project deployment is registered.
    ///
    /// # Errors
    /// Returns database errors.
    fn exists(
        &mut self,
        triple: &Triple,
    ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send;

    /// Record a project without replacing its existing definition.
    ///
    /// # Errors
    /// Returns database errors.
    fn record_project(
        &mut self,
        org: &crate::OrgId,
        project: &crate::ProjectId,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send;

    /// Find a deployment target by name.
    ///
    /// # Errors
    /// Returns database errors.
    fn target_id(
        &mut self,
        name: &str,
    ) -> impl std::future::Future<Output = Result<Option<String>, Self::Error>> + Send;

    /// Record a deployment and return its preserved or newly minted instance suffix.
    ///
    /// # Errors
    /// Returns database errors.
    fn record(
        &mut self,
        registration: &Registration<'_>,
    ) -> impl std::future::Future<Output = Result<InstanceSuffix, Self::Error>> + Send;
}

/// Organization changes held in one transaction.
pub trait OrganizationLock {
    type Error;

    /// Serialize changes to an organization's initial administrator.
    ///
    /// # Errors
    /// Returns a database error if the organization is absent or cannot be locked.
    fn lock_org(
        &mut self,
        org: &crate::OrgId,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>> + Send;
}
