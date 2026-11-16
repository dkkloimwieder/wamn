//! Shared release fixture identity for local generation.

use std::collections::BTreeSet;

use wamn_catalog::PackageCoordinate;

/// The release inputs that an application fixture provisions and publishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseIdentity {
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub environment: wamn_registry::EnvName,
    pub route_host: String,
    pub packages: BTreeSet<PackageCoordinate>,
}

impl ReleaseIdentity {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> wamn_registry::ProjectEnvKey {
        wamn_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
}
