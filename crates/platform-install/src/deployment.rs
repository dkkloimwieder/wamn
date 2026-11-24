//! Cluster operations used by environment deployment.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;

use serde_json::Value;
use tokio::time::Instant;
use wamn_registry::{ApplicationName, Triple};

use crate::database::ConnectionConfig;

/// The pending host upgrade after preparation completes.
pub type Upgrade<'a> = Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>>;

/// A selected chart and its image references.
pub trait Chart: Debug + Send + Sync {
    fn directory(&self) -> &Path;
    fn is_downloaded(&self) -> bool;
    fn image(&self, name: &str) -> anyhow::Result<&str>;
}

/// Prepare the mounted credentials for a database connection.
#[async_trait::async_trait]
pub trait DatabaseFiles: Debug + Send + Sync {
    async fn materialize(&self, configuration: &ConnectionConfig) -> anyhow::Result<()>;
}

/// Read and apply the cluster resources that environment deployment uses.
#[async_trait::async_trait]
pub trait Cluster: Debug + Send + Sync {
    fn namespace(&self) -> &str;

    async fn prepare_host_upgrade(
        &self,
        chart: &Path,
        name: String,
        values: &Value,
    ) -> anyhow::Result<Upgrade<'_>>;
    async fn host_image(&self, chart: &Path) -> anyhow::Result<String>;
    async fn host_pods(&self, name: &str) -> anyhow::Result<Value>;
    async fn postgres_settings(&self) -> anyhow::Result<Value>;
    async fn apply_project(
        &self,
        chart: &Path,
        release: &str,
        values: &Value,
    ) -> anyhow::Result<()>;
    async fn update_project(
        &self,
        chart: &Path,
        release: &str,
        values: Value,
    ) -> anyhow::Result<()>;
    async fn wait_database(
        &self,
        owner: &ConnectionConfig,
        capture: &ConnectionConfig,
    ) -> anyhow::Result<()>;

    async fn wait_reader_stopped(&self, triple: &Triple) -> anyhow::Result<()>;
    async fn wait_reader_ready(&self, triple: &Triple) -> anyhow::Result<()>;
    async fn reader_exists(&self, triple: &Triple) -> anyhow::Result<bool>;
    async fn publication_exists(&self, database: &str) -> anyhow::Result<bool>;

    async fn write_credentials(&self, name: &str, key: &str, bytes: &[u8]) -> anyhow::Result<()>;
    async fn read_credentials(&self, name: &str, key: &str) -> anyhow::Result<Option<Vec<u8>>>;
    async fn delete_secret(&self, name: &str) -> anyhow::Result<bool>;

    async fn workload_annotations(
        &self,
        triple: &Triple,
        application: &ApplicationName,
    ) -> anyhow::Result<Option<BTreeMap<String, String>>>;
    async fn applications(&self, triple: &Triple) -> anyhow::Result<Vec<ApplicationName>>;
    async fn wait_workload_ready(&self, name: &str, timeout: Duration) -> anyhow::Result<()>;
    async fn current_job(&self) -> anyhow::Result<Value>;
    async fn apply_application(
        &self,
        chart: &Path,
        name: &str,
        values: &serde_yaml::Value,
        deadline: Instant,
    ) -> anyhow::Result<()>;
    async fn release_manifest(
        &self,
        release: &str,
        revision: u32,
    ) -> anyhow::Result<Option<Vec<u8>>>;
    async fn release_values(&self, release: &str, revision: u32)
    -> anyhow::Result<Option<Vec<u8>>>;
    async fn rollback(&self, release: &str, revision: u32) -> anyhow::Result<()>;
    async fn uninstall(&self, release: &str) -> anyhow::Result<()>;

    async fn require_ownership(&self, triple: &Triple, database: &str) -> anyhow::Result<()>;
    async fn delete_reader(
        &self,
        chart: &Path,
        triple: &Triple,
        release: Option<&str>,
    ) -> anyhow::Result<()>;
    async fn project_release_exists(&self, release: &str) -> anyhow::Result<bool>;
    async fn remove_database(&self, chart: &Path, release: &str, data: bool) -> anyhow::Result<()>;
    async fn delete_kept_project_resources(
        &self,
        triple: &Triple,
        release: &str,
    ) -> anyhow::Result<Vec<String>>;
    async fn delete_database_certificate_secrets(
        &self,
        database: &str,
        logins: &[&str],
    ) -> anyhow::Result<()>;
    async fn database_exists(&self, database: &str) -> anyhow::Result<bool>;
    async fn database_cluster_exists(&self, name: &str) -> anyhow::Result<bool>;
}
