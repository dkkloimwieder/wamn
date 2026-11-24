//! Database operations and guarded sessions used by deployment.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use std::path::Path;

use chrono::{DateTime, Utc};
use wamn_application::application_config::ApplicationConfig;
use wamn_component_policy::PackageManifest;
use wamn_digest::Digest;
use wamn_package::{
    PackageName,
    canonical_name::CanonicalName,
    package_schema::{AppliedPackage, PackageDirectory},
};
use wamn_platform_install::{database::ConnectionConfig, workload_role::WorkloadRoleFamily};
use wamn_registry::{
    ApplicationName, EnvName, EnvironmentRow, OrgId, ProjectEnvKey, ProjectId, Triple,
    configuration::EnvironmentConfiguration, instance_suffix::InstanceSuffix,
    platform_domain::PlatformDomain,
};

use super::{
    analyse::ProjectState, capture_gap::ClosedCaptureGap, document::EnvironmentDocument,
    drain::UnfinishedRun,
};
use crate::{
    apply_package::{ApplyOutcome, ApplyPackageRequest},
    configuration::SequenceDefault,
    migrations::SchemaReport,
};

#[derive(Debug)]
pub enum DatabaseConnection<'a> {
    Direct(&'a str),
    WaitForEnvironment { input: &'a str, triple: &'a Triple },
    Wait { input: &'a str, database: &'a str },
    Delete { input: &'a str, data: bool },
}

#[derive(Debug)]
pub struct PendingCaptureGap<'a> {
    pub triple: &'a Triple,
    pub name: &'a str,
    pub reason: &'a str,
    pub start_lsn: Option<&'a str>,
    pub start_at: DateTime<Utc>,
    pub end_lsn: &'a str,
}

#[derive(Debug)]
pub struct CaptureRegistration {
    pub name: String,
    pub stream: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct CaptureStatus {
    pub reason: String,
    pub confirmed: Option<String>,
}

#[derive(Debug)]
pub struct ConnectionBinding {
    pub id: String,
    pub integration_type: String,
    pub parameters: serde_json::Value,
    pub generation: i64,
    pub target: String,
    pub authority: Option<String>,
}

#[derive(Debug)]
pub struct IntegrationBinding {
    pub integration_type: String,
    pub runtime: String,
    pub schema: Option<serde_json::Value>,
    pub digest: Option<String>,
    pub exclusive: bool,
    pub exports: Vec<String>,
    pub write_interfaces: Vec<String>,
    pub placement: String,
}

#[async_trait::async_trait]
pub trait PackageMigrations: Send {
    async fn observe(
        &mut self,
        path: &Path,
        key: &ProjectEnvKey,
        id: &PackageName,
    ) -> anyhow::Result<SchemaReport>;
}

#[async_trait::async_trait]
pub trait CaptureRepair: Send {
    async fn repair(&mut self, name: &str) -> anyhow::Result<String>;
}

#[async_trait::async_trait]
pub trait Database: Debug + Send + Sync {
    async fn lock(
        &self,
        connection: DatabaseConnection<'_>,
        org: &OrgId,
        env: &EnvName,
    ) -> anyhow::Result<LifecycleLock>;
    fn unreachable(&self, error: &anyhow::Error) -> bool;

    async fn read_configuration(
        &self,
        input: &str,
        triple: &Triple,
    ) -> anyhow::Result<Option<EnvironmentConfiguration>>;
    async fn environment_row(
        &self,
        input: &str,
        triple: &Triple,
    ) -> anyhow::Result<Option<EnvironmentRow>>;
    async fn read_project(&self, url: &str) -> anyhow::Result<Option<ProjectState>>;
    async fn unfinished(
        &self,
        url: &str,
        database: &str,
        env: &EnvName,
        app: Option<&ApplicationName>,
    ) -> anyhow::Result<Vec<UnfinishedRun>>;
    async fn installed_packages(
        &self,
        input: &str,
    ) -> anyhow::Result<Vec<(PackageName, Option<Digest>)>>;
    async fn package_baselines(
        &self,
        input: &str,
    ) -> anyhow::Result<BTreeMap<PackageName, AppliedPackage>>;
    async fn package_migrations(
        &self,
        input: Option<&str>,
    ) -> anyhow::Result<Box<dyn PackageMigrations>>;
    async fn apply_package(
        &self,
        request: ApplyPackageRequest,
        directory: PackageDirectory,
        path: &Path,
    ) -> anyhow::Result<ApplyOutcome>;
    async fn materialize_application_principals(
        &self,
        domain: &PlatformDomain,
        url: &str,
        key: &ProjectEnvKey,
        packages: &BTreeMap<PackageName, PackageManifest>,
    ) -> anyhow::Result<()>;
    fn migration_configuration(&self, owner: &str) -> anyhow::Result<String>;

    async fn prepare_cdc(
        &self,
        owner: &str,
        capture: &str,
        database: &str,
        publication: &str,
        schemas: &[CanonicalName],
    ) -> anyhow::Result<()>;
    async fn capture_exists(
        &self,
        owner: &ConnectionConfig,
        name: &str,
        data: bool,
    ) -> anyhow::Result<bool>;
    async fn drop_capture(
        &self,
        owner: &ConnectionConfig,
        name: &str,
        data: bool,
    ) -> anyhow::Result<()>;
    async fn inspect_capture(
        &self,
        url: &str,
        database: &str,
        name: &str,
        triple: &Triple,
    ) -> anyhow::Result<(CaptureStatus, Box<dyn CaptureRepair>)>;
}

#[async_trait::async_trait]
pub trait DatabaseSession: Debug + Send + Sync {
    async fn release(self: Box<Self>);

    async fn reserve(
        &self,
        projects: &[EnvironmentDocument],
        disposable: Option<bool>,
    ) -> anyhow::Result<Vec<InstanceSuffix>>;
    async fn retain_integrations(
        &self,
        org: &OrgId,
        env: &EnvName,
        names: &[&str],
    ) -> anyhow::Result<()>;
    async fn project_count(&self, org: &OrgId, env: &EnvName) -> anyhow::Result<u32>;
    async fn host_projects(
        &self,
        org: &OrgId,
        env: &EnvName,
        reserved: &[&str],
        excluded: Option<&ProjectId>,
        drain: Option<i32>,
    ) -> anyhow::Result<(Vec<ProjectId>, i32)>;
    async fn connection(&self, triple: &Triple) -> anyhow::Result<ConnectionConfig>;
    async fn project_schema(
        &self,
        triple: &Triple,
        migration: &str,
        apply: bool,
        output: &mut Vec<u8>,
    ) -> anyhow::Result<()>;
    async fn read_configuration(
        &self,
        triple: &Triple,
    ) -> anyhow::Result<Option<EnvironmentConfiguration>>;
    async fn fill(&self, owner: &str, triple: &Triple, issuer: &str) -> anyhow::Result<()>;
    async fn save_deployment(
        &self,
        triple: &Triple,
        configuration: &EnvironmentConfiguration,
        publication: &str,
        schemas: &[CanonicalName],
        capture: &ConnectionConfig,
    ) -> anyhow::Result<()>;
    async fn environment_row(&self, triple: &Triple) -> anyhow::Result<Option<EnvironmentRow>>;
    async fn application_names(&self, triple: &Triple)
    -> anyhow::Result<BTreeSet<ApplicationName>>;
    async fn capture_instance(&self, triple: &Triple) -> anyhow::Result<InstanceSuffix>;
    async fn instance(&self, triple: &Triple) -> anyhow::Result<InstanceSuffix>;
    async fn disposable(&self, triple: &Triple) -> anyhow::Result<bool>;
    async fn delete_inputs(&self, triple: &Triple)
    -> anyhow::Result<Option<(InstanceSuffix, i32)>>;
    async fn delete_rows(&self, triple: &Triple) -> anyhow::Result<Vec<String>>;
    async fn reset(&self, owner: &str, migration: &str, triple: &Triple) -> anyhow::Result<()>;

    async fn initialize_project(
        &self,
        database: &str,
        owner: &str,
        unconfigured: bool,
    ) -> anyhow::Result<()>;
    async fn grant_workload(
        &self,
        connections: &[(WorkloadRoleFamily, ConnectionConfig)],
    ) -> anyhow::Result<()>;
    async fn unfinished(
        &self,
        env: &EnvName,
        app: Option<&ApplicationName>,
    ) -> anyhow::Result<Vec<UnfinishedRun>>;
    async fn installed_packages(&self) -> anyhow::Result<Vec<(PackageName, Option<Digest>)>>;
    async fn disable_connection_principals(&self) -> anyhow::Result<()>;
    async fn platform_domain(&self) -> anyhow::Result<PlatformDomain>;
    async fn check_workflows(&self, application: &ApplicationConfig) -> anyhow::Result<()>;
    async fn create_number_sequences(&self, sequences: &[SequenceDefault]) -> anyhow::Result<()>;

    async fn environment_target(&self, triple: &Triple) -> anyhow::Result<Option<Option<String>>>;
    async fn connection_binding(&self, name: &str) -> anyhow::Result<Option<ConnectionBinding>>;
    async fn integration_binding(
        &self,
        triple: &Triple,
        target: &str,
        version: i32,
        connection: &ConnectionBinding,
    ) -> anyhow::Result<Option<IntegrationBinding>>;

    async fn has_capture_gap(&self, triple: &Triple) -> anyhow::Result<bool>;
    async fn capture_registration(&self, triple: &Triple) -> anyhow::Result<CaptureRegistration>;
    async fn record_capture_gap(
        &self,
        gap: &PendingCaptureGap<'_>,
    ) -> anyhow::Result<DateTime<Utc>>;
    async fn close_capture_gap(&self, triple: &Triple) -> anyhow::Result<ClosedCaptureGap>;
}

/// The database session and its loss signal. The owner releases it after protected work.
#[derive(Debug)]
pub struct LifecycleLock {
    pub session: Box<dyn DatabaseSession>,
    pub lost: tokio::sync::watch::Receiver<bool>,
}

impl LifecycleLock {
    pub async fn release(self) {
        self.session.release().await;
    }

    pub async fn hold<T>(
        database: &dyn Database,
        connection: DatabaseConnection<'_>,
        triple: &Triple,
        operation: impl AsyncFnOnce(&dyn DatabaseSession) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let lock = database.lock(connection, &triple.org, &triple.env).await?;
        let result = lock.protect(operation(&*lock.session)).await;
        lock.release().await;
        result
    }

    pub async fn protect<T>(
        &self,
        operation: impl Future<Output = anyhow::Result<T>>,
    ) -> anyhow::Result<T> {
        let mut lost = self.lost.clone();
        tokio::select! {
            biased;
            _ = lost.wait_for(|lost| *lost) => anyhow::bail!("lifecycle lock lost; operation stopped"),
            result = operation => result,
        }
    }
}

/// Reason recorded when a capture position is absent.
pub const MISSING_CAPTURE_REASON: &str = "missing";
