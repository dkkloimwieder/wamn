//! Registry inputs for local artifact publication.

use clap::Args;
use std::path::PathBuf;
use wamn_engine::component_admission::ReleaseComponentAdmission;
use wamn_oci::package_artifact::OCI_CA_PATHS_ENV;
use wamn_oci::package_artifact::PackageRegistry;
use wamn_release::package_artifact::{ComponentPublisher, ComponentPush};
use wamn_release::release_package::RegistryPackagePublisher;

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

pub(crate) fn package_publisher() -> RegistryPackagePublisher<'static> {
    RegistryPackagePublisher::new(&ReleaseComponentAdmission, &StockComponentPublisher)
}

struct StockComponentPublisher;

#[async_trait::async_trait]
impl ComponentPublisher for StockComponentPublisher {
    async fn push(&self, request: ComponentPush<'_>) -> anyhow::Result<wamn_digest::Digest> {
        wash_runtime::oci::set_extra_ca_certificates(request.ca_paths)?;
        let digest = wash_runtime::oci::push_component(
            request.reference,
            request.component.bytes(),
            wash_runtime::oci::OciConfig {
                credentials: request
                    .credentials
                    .map(|(user, password)| (user.to_owned(), password.to_owned())),
                insecure: request.insecure,
                ..Default::default()
            },
            None,
        )
        .await?;
        Ok(digest.parse()?)
    }
}
