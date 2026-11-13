//! Shared release fixture identity and SQLx verification for local generation.

use std::collections::BTreeSet;

use wamn_catalog::PackageCoordinate;

pub mod sqlx;

/// The release inputs that an application fixture provisions and publishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseIdentity {
    pub org: String,
    pub project: String,
    pub environment: String,
    pub route_host: String,
    pub packages: BTreeSet<PackageCoordinate>,
}

impl ReleaseIdentity {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> String {
        wamn_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
}
