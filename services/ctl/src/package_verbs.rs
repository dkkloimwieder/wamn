//! Registry inputs for local artifact publication.

use clap::Args;
use std::path::PathBuf;
use wamn_component_artifact::component_artifact_source::OCI_CA_PATHS_ENV;
use wamn_package_control::package_artifact::PackageRegistry;

/// Registry access for package artifacts.
#[derive(Debug, Args)]
pub struct RegistryArgs {
    /// Explicit `<registry>/<repository>` base for package artifacts.
    #[arg(long)]
    pub artifact_base: String,

    /// `.dockerconfigjson` file carrying the registry credential.
    #[arg(long)]
    pub registry_auth_file: PathBuf,

    /// Use plain HTTP for exactly the registry in `--artifact-base`.
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,

    /// PEM CA bundle trusted for the registry, on top of the compiled-in
    /// roots. Repeat or comma-delimit. Env `WASH_OCI_CA_PATHS`.
    #[arg(long = "oci-ca-path", env = OCI_CA_PATHS_ENV, value_delimiter = ',')]
    pub oci_ca_paths: Vec<PathBuf>,
}

impl RegistryArgs {
    pub(crate) fn registry(&self) -> PackageRegistry {
        PackageRegistry {
            artifact_base: self.artifact_base.clone(),
            credentials: None,
            registry_auth_file: self.registry_auth_file.clone(),
            insecure_registry: self.insecure_registry,
            oci_ca_paths: self.oci_ca_paths.clone(),
        }
    }
}
