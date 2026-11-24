//! Read verified package inputs before passing the SQL work to Postgres.

pub use wamn_release::package_source::PackageRoot;

pub async fn apply_package(
    platform: &crate::environment::Platform,
    request: ApplyPackageRequest,
) -> anyhow::Result<ApplyOutcome> {
    let directory = platform.packages.read_package_root(&request.package)?;
    let migration_root = platform.packages.migration_root(&request.package.path);
    platform
        .database
        .apply_package(request, directory, &migration_root)
        .await
}

/// Inputs of one exact package application.
#[derive(Debug)]
pub struct ApplyPackageRequest {
    /// Package root containing strict wamn.json and migrations/, with the
    /// digest of the verified artifact it was opened from.
    pub package: PackageRoot,

    /// Owner connection to the target project-environment database.
    pub database_url: String,

    /// Direct restricted migration login; never the catalog owner's credential.
    pub migration_database_url: String,

    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub environment: wamn_registry::EnvName,
}

impl ApplyPackageRequest {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> wamn_registry::ProjectEnvKey {
        wamn_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
}

/// Result of one exact package application.
#[derive(Debug)]
pub struct ApplyOutcome {
    /// Package id of the applied coordinate.
    pub package_id: wamn_package::PackageName,

    /// Package version of the applied coordinate.
    pub package_version: wamn_package::package_version::PackageVersion,

    /// Number of pending migrations this application ran.
    pub migrations_applied: usize,

    /// Whether this application changed the database.
    pub changed: bool,
}
