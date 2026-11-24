//! Independent environment provisioning, initialization, and targeted application activation.

pub mod analyse;
pub mod bindings;
mod broker;
pub mod capture_gap;
pub mod chart_values;
pub mod cluster;
pub mod database;
pub mod delete;
pub mod document;
pub mod drain;
pub mod ensure;
pub mod lifecycle;
pub mod operation;
pub mod runtime;
pub mod sql;
pub mod stage;
pub mod worker;
pub mod write;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wamn_identity_protocol::nats::BrokerIdentity;

use cluster::{Chart, Cluster, DatabaseFiles};

type DatabaseValues = fn(
    &wamn_platform_install::database::ConnectionConfig,
    u32,
    serde_json::Value,
    &serde_json::Value,
) -> anyhow::Result<serde_json::Value>;

type ChartSelector = fn(&Path) -> anyhow::Result<Arc<dyn Chart>>;

/// The platform inputs of the lifecycle verbs.
#[derive(Debug)]
pub struct Platform {
    /// The system database URL selected for this verb.
    pub system_database_url: String,
    pub database_values: DatabaseValues,
    pub database: Arc<dyn database::Database>,
    /// The operation worker's writer for database credentials.
    pub database_files: Option<Arc<dyn DatabaseFiles>>,
    /// The cluster, its context and the operator release's namespace (R6).
    pub cluster: Arc<dyn Cluster>,
    /// A stamped release chart directory.
    pub chart: PathBuf,
    /// Keep fetched chart files alive for this operation.
    pub chart_download: Option<Arc<dyn Chart>>,
    pub packages: Arc<dyn wamn_release::package_source::PackageSource>,
    pub select_chart: ChartSelector,
    pub registry_token_metadata: bool,
    pub oci_ca_paths: Vec<PathBuf>,
    /// Registry transport selected by the installed platform host.
    pub insecure_registry: bool,
    /// `<registry>/<repository>` of package artifacts. Needed only when a
    /// release names a package the environment does not have yet, or the
    /// document pins a web artifact.
    pub package_artifact_base: Option<String>,
    /// A YAML mapping: the platform's part of the host group body (replicas,
    /// resources, registry mounts). The verb adds the environment's
    /// credential Secret references.
    pub host_group: Option<PathBuf>,
    /// The identity issuer address used for project user rows.
    pub identity_issuer: String,
    pub broker_identity: fn(&str) -> anyhow::Result<Box<dyn BrokerIdentity>>,
    /// The shared in-cluster NATS endpoint.
    pub nats_url: String,
    /// Provider for the host's OAuth wrapping key.
    pub oauth_encryption: Option<wamn_envelope::encryption::Configuration>,
    /// The event broker for application capture and entity consumers.
    pub events: Option<wamn_event::deploy_broker::EventBroker>,
    pub broker: Arc<dyn wamn_event::deploy_broker::DeploymentBroker>,
}

impl Platform {
    /// Stage this database's credentials when running in an operation Job.
    pub async fn prepare_database(
        &self,
        config: &wamn_platform_install::database::ConnectionConfig,
    ) -> anyhow::Result<()> {
        if let Some(files) = &self.database_files {
            files.materialize(config).await?;
        }
        Ok(())
    }
}
