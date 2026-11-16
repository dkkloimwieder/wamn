//! Independent environment provisioning, initialization, and targeted application activation.

pub mod analyse;
pub mod bindings;
pub mod capture_gap;
pub mod connections;
pub mod delete;
pub mod document;
pub mod drain;
pub mod ensure;
pub mod lifecycle;
pub mod runtime;
pub mod show;
pub mod sql;
pub mod stage;
pub mod write;

use std::path::PathBuf;

use wamn_identity_client::PatIssuerConfig;
use wamn_release_control::release_chart;

/// The platform inputs of the lifecycle verbs.
#[derive(Debug, Clone)]
pub struct Platform {
    /// The system database URL selected for this verb.
    pub system_database_url: String,
    /// The cluster, its context and the operator release's namespace (R6).
    pub target: release_chart::Target,
    /// A stamped release chart directory.
    pub chart: PathBuf,
    /// Keep fetched chart files alive for this operation.
    pub chart_download: Option<std::sync::Arc<crate::platform::artifact_plan::SelectedChart>>,
    pub registry_auth_file: PathBuf,
    pub registry_token_metadata: bool,
    pub oci_ca_paths: Vec<PathBuf>,
    /// Registry transport selected by the installed platform host.
    pub insecure_registry: bool,
    /// `<registry>/<repository>` of package artifacts. Needed only when a
    /// release names a package the environment does not have yet, or the
    /// document pins a web artifact.
    pub package_artifact_base: Option<String>,
    /// The namespace of the CloudNativePG `Cluster`, which the environment's
    /// `Database` CR shares.
    pub database_namespace: String,
    /// The host and port every credential URL names. Absent host: the target
    /// Service `<cluster>-rw.<database_namespace>.svc.cluster.local`.
    pub database_host: Option<String>,
    pub database_port: u16,
    /// A YAML mapping: the platform's part of the host group body (replicas,
    /// resources, registry mounts). The verb adds the environment's
    /// credential Secret references.
    pub host_group: Option<PathBuf>,
    /// The identity service that issues the management-author PAT. Without
    /// it, `apply` issues no PAT.
    pub pat_issuer: PatIssuerConfig,
    /// The event broker for application capture and entity consumers.
    pub events: Option<wamn_broker::EventBroker>,
}

impl Platform {
    pub(crate) async fn registry(
        &self,
        base: &str,
    ) -> anyhow::Result<wamn_package_artifact::PackageRegistry> {
        let credentials = if self.registry_token_metadata {
            let base = wamn_catalog::parse_component_artifact_base(base)?;
            Some(wamn_package_artifact::registry_credentials::RegistryCredentialSource::MetadataServer
                .credentials(base.registry()).await?)
        } else {
            None
        };
        Ok(wamn_package_artifact::PackageRegistry {
            artifact_base: base.to_owned(),
            credentials,
            registry_auth_file: self.registry_auth_file.clone(),
            insecure_registry: self.insecure_registry,
            oci_ca_paths: self.oci_ca_paths.clone(),
        })
    }
}
