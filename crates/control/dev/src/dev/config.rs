//! Strict deployment-owned configuration for the development loop.
//!
//! This module validates and preflights externally supplied services. It does
//! not provision them or execute any development stage.

pub(super) mod plan;
use plan::package_source_matches;
pub use plan::{dev_config_schema, dev_config_schema_bytes};

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use crate::identifier::Identifier;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout_at};
use tokio_postgres::Config as PostgresConfig;
use url::Url;
use wamn_compiler::manifest::PackageManifest;

use super::activation::DevActivationIdentity;

/// Whole-startup budget shared by every configured reachability probe.
pub const STARTUP_REACHABILITY_BUDGET: Duration = Duration::from_secs(5);

const DOCUMENT_KEY: &str = "$";
const TARGET_DATABASE_URL: &str = "target_database_url";
const TARGET_PRIVILEGES_FILE: &str = "target_privileges_file";
const TARGET_TEMPLATE_DATABASE: &str = "target_template_database";
const TARGET_DATABASE_ACL_FILE: &str = "target_database_acl_file";
const SYSTEM_DATABASE_URL: &str = "system_database_url";
const IDENTITY_DATABASE_URL: &str = "identity_database_url";
const GUEST_DATABASE_URL: &str = "guest_database_url";
const EXECUTOR_PLATFORM_DATABASE_URL: &str = "executor_platform_database_url";
const HTTP_ADMITTER_DATABASE_URL: &str = "http_admitter_database_url";
const EVENT_MATERIALIZER_DATABASE_URL: &str = "event_materializer_database_url";
const ADMINISTRATION_DATABASE_URL: &str = "administration_database_url";
const SCHEDULER_NATS_URL: &str = "scheduler_nats_url";
const NATS_TLS_CA: &str = "nats_tls_ca";
const NATS_TLS_CERT: &str = "nats_tls_cert";
const NATS_TLS_KEY: &str = "nats_tls_key";
const STREAM_REPLICAS: &str = "stream_replicas";
const DUP_WINDOW_SECS: &str = "dup_window_secs";
const OTEL_EXPORTER_OTLP_ENDPOINT: &str = "otel_exporter_otlp_endpoint";
const CONDITION_BEARER_TOKEN: &str = "condition_bearer_token";
const ROUTE_HOST: &str = "route_host";
const PLATFORM_DOMAIN: &str = "platform_domain";
const LOCAL_ARTIFACTS: &str = "local_artifacts";
const PACKAGE_SOURCES: &str = "package_sources";
const CATALOG: &str = "catalog";
const ENVIRONMENT: &str = "environment";
const ORG: &str = "org";
const PROJECT: &str = "project";
const HOST_GROUP: &str = "host_group";
const HOST_NAME: &str = "host_name";
const RUNNER: &str = "runner";
const HOST_BINARY: &str = "host_binary";
const WASMTIME_CACHE_DIR: &str = "wasmtime_cache_dir";
const HOST_CONFIG_FILE: &str = "host_config_file";
const CDC_READER: &str = "cdc_reader";
#[cfg(test)]
const PACKAGE_MANIFEST_FILE: &str = "wamn.json";

pub(super) const POSTGRES_SYSTEM_DATABASES: [&str; 3] = ["postgres", "template0", "template1"];
const POSTGRES_ROUTING_QUERY_KEYS: [&str; 5] = ["host", "hostaddr", "port", "dbname", "user"];

/// Explicit local candidate files owned by one disposable development session.
#[derive(Clone, Debug, Deserialize, serde::Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalArtifacts {
    pub directory: PathBuf,
    pub flow_http_component: PathBuf,
    /// The built platform materializer, which the loop runs beside the
    /// release as the cluster does.
    pub materializer_component: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bindings: Option<PathBuf>,
}

/// The CDC reader the loop runs beside the host: the cluster's binary with the
/// cluster's registration, credentials and broker identity.
#[derive(Clone, Deserialize, serde::Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CdcReader {
    /// The built `wamn-cdc-reader`.
    pub binary: PathBuf,
    /// The registry-reader generation that reads the registration.
    pub system_database_url: String,
    /// The replication credential, the `url` of the rendered CDC Secret.
    pub cdc_url: String,
    /// The CDC SQL of `enable-cdc-project-env`, applied to every new target.
    pub cdc_sql_file: PathBuf,
}

impl fmt::Debug for CdcReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CdcReader")
            .field("binary", &self.binary)
            .field("system_database_url", &"[REDACTED]")
            .field("cdc_url", &"[REDACTED]")
            .field("cdc_sql_file", &self.cdc_sql_file)
            .finish()
    }
}

/// Public trust for the identity process owned by the development environment.
#[derive(Clone, Debug, Deserialize, serde::Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionIdentity {
    pub issuer: String,
    pub ca: PathBuf,
    pub instance_suffix: String,
}

/// Sole field authority for the strict deployment-owned `dev.json` document.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DevConfigDocument {
    target_database_url: String,
    target_privileges_file: PathBuf,
    target_template_database: String,
    target_database_acl_file: PathBuf,
    system_database_url: String,
    identity_database_url: String,
    #[serde(default)]
    #[schemars(with = "String")]
    connection_database_url: Option<String>,
    #[serde(default)]
    session_identity: Option<SessionIdentity>,
    guest_database_url: String,
    executor_platform_database_url: String,
    http_admitter_database_url: String,
    event_materializer_database_url: String,
    administration_database_url: String,
    scheduler_nats_url: String,
    nats_tls_ca: PathBuf,
    nats_tls_cert: PathBuf,
    nats_tls_key: PathBuf,
    stream_replicas: usize,
    dup_window_secs: u64,
    otel_exporter_otlp_endpoint: String,
    condition_bearer_token: String,
    route_host: String,
    platform_domain: String,
    local_artifacts: LocalArtifacts,
    package_sources: Vec<PathBuf>,
    catalog: String,
    environment: String,
    org: String,
    project: String,
    host_group: String,
    host_name: String,
    runner: String,
    host_binary: PathBuf,
    wasmtime_cache_dir: PathBuf,
    /// The materializer's events binding in the environment's private root,
    /// as the cluster mounts its Secret.
    host_config_file: PathBuf,
    /// The CDC reader that `wamn dev up` enabled. Absent means no reader runs.
    #[serde(default)]
    cdc_reader: Option<CdcReader>,
}

/// Stable category of a development configuration refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevConfigErrorType {
    MalformedDocument,
    UnknownKey,
    MissingKey,
    InvalidValue,
    DatabaseCollision,
    EndpointUnreachable,
}

impl DevConfigErrorType {
    /// Stable diagnostic code for this error category.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MalformedDocument => "dev-config-malformed",
            Self::UnknownKey => "dev-config-unknown-key",
            Self::MissingKey => "dev-config-missing-key",
            Self::InvalidValue => "dev-config-invalid-value",
            Self::DatabaseCollision => "dev-config-database-collision",
            Self::EndpointUnreachable => "dev-config-endpoint-unreachable",
        }
    }
}

/// Refusal to load or preflight the deployment-owned development config.
#[derive(Debug)]
pub struct DevConfigError {
    type_: DevConfigErrorType,
    key: Box<str>,
    endpoint: Option<Box<str>>,
    detail: &'static str,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl DevConfigError {
    fn new(type_: DevConfigErrorType, key: impl Into<Box<str>>, detail: &'static str) -> Self {
        Self {
            type_,
            key: key.into(),
            endpoint: None,
            detail,
            source: None,
        }
    }

    fn endpoint(
        type_: DevConfigErrorType,
        key: &'static str,
        endpoint: impl Into<Box<str>>,
        detail: &'static str,
    ) -> Self {
        Self {
            type_,
            key: key.into(),
            endpoint: Some(endpoint.into()),
            detail,
            source: None,
        }
    }

    fn with_source(mut self, source: impl Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Stable refusal category.
    pub const fn error_type(&self) -> DevConfigErrorType {
        self.type_
    }

    /// Exact JSON key that owns the refusal.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Credential-free endpoint, when the refusal concerns an endpoint.
    pub fn sanitized_endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }
}

impl fmt::Display for DevConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at config key {:?}",
            self.type_.as_str(),
            self.key
        )?;
        if let Some(endpoint) = &self.endpoint {
            write!(formatter, " ({endpoint})")?;
        }
        write!(formatter, ": {}", self.detail)
    }
}

impl Error for DevConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

/// Stable category of a package-source or component-integrity refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevPackageErrorType {
    ManifestRead,
    ManifestInvalid,
    BaseDependencyMissing,
    BaseDependencyAmbiguous,
}

impl DevPackageErrorType {
    /// Stable diagnostic code for this error category.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ManifestRead => "dev-package-manifest-read",
            Self::ManifestInvalid => "dev-package-manifest-invalid",
            Self::BaseDependencyMissing => "dev-base-dependency-missing",
            Self::BaseDependencyAmbiguous => "dev-base-dependency-ambiguous",
        }
    }
}

/// Refusal to resolve a manifest-declared package.
#[derive(Debug)]
pub struct DevPackageError {
    type_: DevPackageErrorType,
    manifest_path: Option<PathBuf>,
    coordinate: Option<Box<str>>,
    dependency_digest: Option<Box<str>>,
    searched_roots: Box<[PathBuf]>,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl DevPackageError {
    fn manifest(
        type_: DevPackageErrorType,
        manifest_path: PathBuf,
        source: impl Into<Box<dyn Error + Send + Sync>>,
    ) -> Self {
        Self {
            type_,
            manifest_path: Some(manifest_path),
            coordinate: None,
            dependency_digest: None,
            searched_roots: Box::new([]),
            source: Some(source.into()),
        }
    }

    fn dependency(
        type_: DevPackageErrorType,
        coordinate: impl Into<Box<str>>,
        dependency_digest: impl Into<Box<str>>,
        searched_roots: &[PathBuf],
    ) -> Self {
        Self {
            type_,
            manifest_path: None,
            coordinate: Some(coordinate.into()),
            dependency_digest: Some(dependency_digest.into()),
            searched_roots: searched_roots.into(),
            source: None,
        }
    }

    /// Stable refusal category.
    pub const fn error_type(&self) -> DevPackageErrorType {
        self.type_
    }

    /// Manifest path that could not be read or parsed.
    pub fn manifest_path(&self) -> Option<&Path> {
        self.manifest_path.as_deref()
    }

    /// Exact `package@version` coordinate involved in dependency resolution.
    pub fn coordinate(&self) -> Option<&str> {
        self.coordinate.as_deref()
    }

    /// Manifest-declared component digest expected for this dependency.
    pub fn dependency_digest(&self) -> Option<&str> {
        self.dependency_digest.as_deref()
    }

    /// Complete list of candidate roots searched for this dependency.
    pub fn searched_roots(&self) -> &[PathBuf] {
        &self.searched_roots
    }
}

impl fmt::Display for DevPackageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.type_.as_str())?;
        if let Some(path) = &self.manifest_path {
            write!(formatter, " at {}", path.display())?;
        }
        if let Some(coordinate) = &self.coordinate {
            write!(formatter, " for {coordinate}")?;
        }
        if let Some(digest) = &self.dependency_digest {
            write!(formatter, " expecting component digest {digest}")?;
        }
        if !self.searched_roots.is_empty()
            || matches!(
                self.type_,
                DevPackageErrorType::BaseDependencyMissing
                    | DevPackageErrorType::BaseDependencyAmbiguous
            )
        {
            write!(formatter, "; searched roots={:?}", self.searched_roots)?;
        }
        Ok(())
    }
}

impl Error for DevPackageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

/// Manifest-owned component digest a built base component is compared with.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BaseComponentDigestExpectation {
    coordinate: Box<str>,
    expected: Box<str>,
}

impl BaseComponentDigestExpectation {
    /// Exact package coordinate that owns this component expectation.
    pub fn coordinate(&self) -> &str {
        &self.coordinate
    }

    /// Manifest-declared component digest.
    pub fn expected(&self) -> &str {
        &self.expected
    }

    /// Carry a built component digest and record the pin it moved off.
    pub fn verify(&self, observed: impl Into<Box<str>>) -> VerifiedBaseComponentDigest {
        let observed = observed.into();
        let superseded_pin = (observed != self.expected).then(|| self.expected.clone());
        VerifiedBaseComponentDigest {
            coordinate: self.coordinate.clone(),
            digest: observed,
            superseded_pin,
        }
    }
}

/// The base component digest one run validated, and the pin it moved off.
///
/// `digest` is always the digest OBSERVED on the built component, which is the
/// value every later stage carries. `superseded_pin` records the stale
/// manifest declaration a disposable run accepted, so the drift is visible in
/// the run without writing anything back to the authored manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedBaseComponentDigest {
    coordinate: Box<str>,
    digest: Box<str>,
    superseded_pin: Option<Box<str>>,
}

impl VerifiedBaseComponentDigest {
    /// Exact base-package coordinate whose component was verified.
    pub fn coordinate(&self) -> &str {
        &self.coordinate
    }

    /// Digest observed on the built component and carried by every later stage.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Manifest pin this run moved off, present only when the digest drifted.
    pub fn superseded_pin(&self) -> Option<&str> {
        self.superseded_pin.as_deref()
    }
}

/// One manifest-declared base package resolved from explicit local candidates.
#[derive(Clone, Debug)]
pub struct ResolvedBasePackage {
    alias: Box<str>,
    root: PathBuf,
    manifest: PackageManifest,
    component_digest: BaseComponentDigestExpectation,
}

impl ResolvedBasePackage {
    /// Overlay-local dependency alias.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Explicit candidate root selected by exact package and version.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Strict manifest parsed from the selected candidate root.
    pub const fn manifest(&self) -> &PackageManifest {
        &self.manifest
    }

    /// Component digest the base build is compared with before Condition.
    pub const fn component_digest(&self) -> &BaseComponentDigestExpectation {
        &self.component_digest
    }
}

/// Strict overlay plus its exact manifest-discovered local base packages.
#[derive(Clone, Debug)]
pub struct ResolvedDevPackages {
    overlay_root: PathBuf,
    overlay_manifest: PackageManifest,
    base_packages: Box<[ResolvedBasePackage]>,
    ignored_package_sources: Box<[PathBuf]>,
}

impl ResolvedDevPackages {
    /// CLI-supplied overlay root.
    pub fn overlay_root(&self) -> &Path {
        &self.overlay_root
    }

    /// Strict overlay manifest that declared every base dependency.
    pub const fn overlay_manifest(&self) -> &PackageManifest {
        &self.overlay_manifest
    }

    /// Resolved base packages in dependency-alias order.
    pub fn base_packages(&self) -> &[ResolvedBasePackage] {
        &self.base_packages
    }

    /// Candidate roots whose package and version matched no dependency.
    pub fn ignored_package_sources(&self) -> &[PathBuf] {
        &self.ignored_package_sources
    }

    /// Number of candidate roots whose package and version matched no dependency.
    pub fn ignored_package_source_count(&self) -> usize {
        self.ignored_package_sources.len()
    }

    /// Read the manifests that generation produced for the selected packages.
    pub(super) fn read_compiled_manifests(&mut self) -> Result<(), DevPackageError> {
        for package in &mut self.base_packages {
            package.manifest =
                read_manifest_file(&wamn_compiler::package_manifest_path(&package.root))?;
        }
        self.overlay_manifest =
            read_manifest_file(&wamn_compiler::package_manifest_path(&self.overlay_root))?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DatabaseIdentity {
    host: Box<str>,
    port: u16,
    database: Box<str>,
    user: Box<str>,
}

impl DatabaseIdentity {
    fn same_database(&self, other: &Self) -> bool {
        self.host == other.host && self.port == other.port && self.database == other.database
    }

    fn same_credential(&self, other: &Self) -> bool {
        self.same_database(other) && self.user == other.user
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReachabilityProbe {
    key: &'static str,
    host: Box<str>,
    port: u16,
    sanitized_endpoint: Box<str>,
}

/// Validated external inputs for one development loop.
#[derive(Clone)]
pub struct DevConfig {
    target_database_url: Box<str>,
    target_privileges_file: PathBuf,
    target_template_database: Box<str>,
    target_database_acl_file: PathBuf,
    system_database_url: Box<str>,
    identity_database_url: Box<str>,
    connection_database_url: Option<String>,
    session_identity: Option<SessionIdentity>,
    guest_database_url: Box<str>,
    executor_platform_database_url: Box<str>,
    http_admitter_database_url: Box<str>,
    event_materializer_database_url: Box<str>,
    administration_database_url: Box<str>,
    scheduler_nats_url: Box<str>,
    nats_tls_ca: PathBuf,
    nats_tls_cert: PathBuf,
    nats_tls_key: PathBuf,
    stream_replicas: usize,
    dup_window_secs: u64,
    otel_exporter_otlp_endpoint: Box<str>,
    local_artifacts: LocalArtifacts,
    condition_bearer_token: Box<str>,
    route_host: Box<str>,
    platform_domain: Box<str>,
    package_sources: Box<[PathBuf]>,
    activation_identity: DevActivationIdentity,
    host_binary: PathBuf,
    wasmtime_cache_dir: PathBuf,
    host_config_file: PathBuf,
    cdc_reader: Option<CdcReader>,
    probes: Box<[ReachabilityProbe]>,
}

impl fmt::Debug for DevConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DevConfig")
            .field(
                TARGET_DATABASE_URL,
                &self.sanitized_endpoint(TARGET_DATABASE_URL),
            )
            .field(TARGET_PRIVILEGES_FILE, &self.target_privileges_file)
            .field(TARGET_TEMPLATE_DATABASE, &self.target_template_database)
            .field(TARGET_DATABASE_ACL_FILE, &self.target_database_acl_file)
            .field(
                SYSTEM_DATABASE_URL,
                &self.sanitized_endpoint(SYSTEM_DATABASE_URL),
            )
            .field(
                IDENTITY_DATABASE_URL,
                &self.sanitized_endpoint(IDENTITY_DATABASE_URL),
            )
            .field(
                GUEST_DATABASE_URL,
                &self.sanitized_endpoint(GUEST_DATABASE_URL),
            )
            .field(
                EXECUTOR_PLATFORM_DATABASE_URL,
                &self.sanitized_endpoint(EXECUTOR_PLATFORM_DATABASE_URL),
            )
            .field(
                HTTP_ADMITTER_DATABASE_URL,
                &self.sanitized_endpoint(HTTP_ADMITTER_DATABASE_URL),
            )
            .field(
                EVENT_MATERIALIZER_DATABASE_URL,
                &self.sanitized_endpoint(EVENT_MATERIALIZER_DATABASE_URL),
            )
            .field(
                ADMINISTRATION_DATABASE_URL,
                &self.sanitized_endpoint(ADMINISTRATION_DATABASE_URL),
            )
            .field(
                SCHEDULER_NATS_URL,
                &self.sanitized_endpoint(SCHEDULER_NATS_URL),
            )
            .field(NATS_TLS_CA, &self.nats_tls_ca)
            .field(NATS_TLS_CERT, &self.nats_tls_cert)
            .field(NATS_TLS_KEY, &self.nats_tls_key)
            .field(STREAM_REPLICAS, &self.stream_replicas)
            .field(DUP_WINDOW_SECS, &self.dup_window_secs)
            .field(
                OTEL_EXPORTER_OTLP_ENDPOINT,
                &self.sanitized_endpoint(OTEL_EXPORTER_OTLP_ENDPOINT),
            )
            .field(LOCAL_ARTIFACTS, &self.local_artifacts)
            .field(CONDITION_BEARER_TOKEN, &"[REDACTED]")
            .field(ROUTE_HOST, &self.route_host)
            .field(PLATFORM_DOMAIN, &self.platform_domain)
            .field(PACKAGE_SOURCES, &self.package_sources)
            .field("activation_identity", &self.activation_identity)
            .field(HOST_BINARY, &self.host_binary)
            .field(WASMTIME_CACHE_DIR, &self.wasmtime_cache_dir)
            .field(HOST_CONFIG_FILE, &self.host_config_file)
            .field(CDC_READER, &self.cdc_reader)
            .finish_non_exhaustive()
    }
}

impl DevConfig {
    /// Trusted issuer inputs for the managed environment, when configured.
    pub fn session_identity(&self) -> Option<&SessionIdentity> {
        self.session_identity.as_ref()
    }

    fn sanitized_endpoint(&self, key: &str) -> &str {
        self.probes
            .iter()
            .find(|probe| probe.key == key)
            .map_or("<not-an-endpoint>", |probe| &probe.sanitized_endpoint)
    }

    /// Target PostgreSQL URL. The shared schema mode controls automatic resets.
    pub fn target_database_url(&self) -> &str {
        &self.target_database_url
    }

    /// Privilege SQL `wamn dev up` emitted for the target database.
    ///
    /// A schema reset drops the target's per-database privileges. Roles are cluster-level and survive, so
    /// this file is what has to be replayed. It is read, never re-derived: a
    /// privilege set the loop computed itself would not be the one the
    /// environment was provisioned with.
    pub fn target_privileges_file(&self) -> &Path {
        &self.target_privileges_file
    }

    /// Pristine template database `wamn dev up` prepared for this target.
    ///
    /// It carries everything a drop destroys except the database-level ACL:
    /// the platform base grants, the run plane and the workload grants. A run clones
    /// it, so a run starts from a database that is pristine by construction
    /// rather than by a replay this module got right.
    pub fn target_template_database(&self) -> &str {
        &self.target_template_database
    }

    /// Database-level ACL `wamn dev up` captured from the healthy target.
    ///
    /// `CREATE DATABASE ... TEMPLATE` copies objects and their ACLs, and does
    /// not copy the ACL of the database itself. This file is that one thing.
    pub fn target_database_acl_file(&self) -> &Path {
        &self.target_database_acl_file
    }

    /// System PostgreSQL URL holding control and identity facts.
    pub fn system_database_url(&self) -> &str {
        &self.system_database_url
    }

    /// Connection-reader PostgreSQL URL passed to the local serving host.
    pub fn connection_database_url(&self) -> Option<&str> {
        self.connection_database_url.as_deref()
    }

    /// Identity-reader PostgreSQL URL passed to the local serving host.
    pub fn identity_database_url(&self) -> &str {
        &self.identity_database_url
    }

    /// Credential-free identity endpoint used in authentication diagnostics.
    pub fn identity_database_endpoint(&self) -> &str {
        self.sanitized_endpoint(IDENTITY_DATABASE_URL)
    }

    /// Guest-SQL PostgreSQL URL passed to the local serving host.
    pub fn guest_database_url(&self) -> &str {
        &self.guest_database_url
    }

    /// Executor-platform PostgreSQL URL passed to the local serving host.
    pub fn executor_platform_database_url(&self) -> &str {
        &self.executor_platform_database_url
    }

    /// Callable-HTTP PostgreSQL URL passed to the local serving host.
    pub fn http_admitter_database_url(&self) -> &str {
        &self.http_admitter_database_url
    }

    /// Event-materializer PostgreSQL URL passed to the local serving host.
    pub fn event_materializer_database_url(&self) -> &str {
        &self.event_materializer_database_url
    }

    /// Administration PostgreSQL URL passed to the local serving host.
    pub fn administration_database_url(&self) -> &str {
        &self.administration_database_url
    }

    /// The one NATS endpoint: the scheduler, the events and the memories
    /// (docs/plan/alignment.md R1, R2).
    pub fn scheduler_nats_url(&self) -> &str {
        &self.scheduler_nats_url
    }

    /// The CA of the local NATS server.
    pub fn nats_tls_ca(&self) -> &Path {
        &self.nats_tls_ca
    }

    /// The one client certificate of the development environment.
    pub fn nats_tls_cert(&self) -> &Path {
        &self.nats_tls_cert
    }

    /// The key of the client certificate.
    pub fn nats_tls_key(&self) -> &Path {
        &self.nats_tls_key
    }

    /// The certificate connection of the platform processes.
    pub fn nats(&self) -> wamn_broker::nats::PlatformNats {
        wamn_broker::nats::PlatformNats {
            nats_url: self.scheduler_nats_url.to_string(),
            nats_tls_ca: self.nats_tls_ca.clone(),
            nats_tls_cert: self.nats_tls_cert.clone(),
            nats_tls_key: self.nats_tls_key.clone(),
            nats_tls_server_name: None,
        }
    }

    /// Declared NATS stream copies, separate from workload instances.
    pub fn stream_replicas(&self) -> usize {
        self.stream_replicas
    }

    /// Declared source-stream duplicate window in seconds.
    pub fn dup_window_secs(&self) -> u64 {
        self.dup_window_secs
    }

    /// OTLP gRPC exporter endpoint passed to the local serving host.
    pub fn otel_exporter_otlp_endpoint(&self) -> &str {
        &self.otel_exporter_otlp_endpoint
    }

    /// Local input for the unpublished candidate.
    pub const fn local_artifacts(&self) -> &LocalArtifacts {
        &self.local_artifacts
    }

    /// Bearer credential presented only to the Condition.
    pub fn condition_bearer_token(&self) -> &str {
        &self.condition_bearer_token
    }

    /// Deployment-owned route hostname supplied at publication.
    pub fn route_host(&self) -> &str {
        &self.route_host
    }

    /// Deployment-owned domain of the platform principal emails.
    pub fn platform_domain(&self) -> &str {
        &self.platform_domain
    }

    /// Explicit local roots considered for manifest-declared base dependencies.
    pub fn package_sources(&self) -> &[PathBuf] {
        &self.package_sources
    }

    /// Deployment identity passed unchanged to local activation.
    pub const fn activation_identity(&self) -> &DevActivationIdentity {
        &self.activation_identity
    }

    /// Explicit local `wamn-host` executable path.
    pub fn host_binary(&self) -> &Path {
        &self.host_binary
    }

    /// Explicit Wasmtime compilation-cache directory for the local host.
    pub fn wasmtime_cache_dir(&self) -> &Path {
        &self.wasmtime_cache_dir
    }

    /// The materializer's events binding, passed to the host as
    /// `--user-config`: the host configuration file with the `plugins` entry.
    pub fn host_config_file(&self) -> &Path {
        &self.host_config_file
    }

    /// The CDC reader the loop starts after each activation, when enabled.
    pub const fn cdc_reader(&self) -> Option<&CdcReader> {
        self.cdc_reader.as_ref()
    }
}

/// Parse one strict deployment-owned JSON document.
pub fn parse_config(bytes: &[u8]) -> Result<DevConfig, DevConfigError> {
    let mut config = plan::parse_config(bytes)?;
    let (target_probe, target_identity) =
        database_probe(TARGET_DATABASE_URL, &config.target_database_url)?;
    let (system_probe, system_identity) =
        database_probe(SYSTEM_DATABASE_URL, &config.system_database_url)?;
    let (identity_probe, identity_identity) =
        database_probe(IDENTITY_DATABASE_URL, &config.identity_database_url)?;
    let (guest_probe, guest_identity) =
        database_probe(GUEST_DATABASE_URL, &config.guest_database_url)?;
    let (executor_platform_probe, executor_platform_identity) = database_probe(
        EXECUTOR_PLATFORM_DATABASE_URL,
        &config.executor_platform_database_url,
    )?;
    let (http_admitter_probe, http_admitter_identity) = database_probe(
        HTTP_ADMITTER_DATABASE_URL,
        &config.http_admitter_database_url,
    )?;
    let (event_materializer_probe, event_materializer_identity) = database_probe(
        EVENT_MATERIALIZER_DATABASE_URL,
        &config.event_materializer_database_url,
    )?;
    let (administration_probe, administration_identity) = database_probe(
        ADMINISTRATION_DATABASE_URL,
        &config.administration_database_url,
    )?;
    validate_disposable_target_database(&target_probe, &target_identity)?;
    validate_runtime_database_credentials(
        &[
            (&target_probe, &target_identity),
            (&system_probe, &system_identity),
        ],
        &[
            (&identity_probe, &identity_identity),
            (&guest_probe, &guest_identity),
            (&executor_platform_probe, &executor_platform_identity),
            (&http_admitter_probe, &http_admitter_identity),
            (&event_materializer_probe, &event_materializer_identity),
            (&administration_probe, &administration_identity),
        ],
    )?;
    let scheduler_probe = url_probe(
        SCHEDULER_NATS_URL,
        &config.scheduler_nats_url,
        &["nats"],
        4222,
        false,
    )?;
    let otel_exporter_probe = url_probe(
        OTEL_EXPORTER_OTLP_ENDPOINT,
        &config.otel_exporter_otlp_endpoint,
        &["http", "https"],
        4317,
        false,
    )?;
    let probes = vec![
        target_probe,
        system_probe,
        identity_probe,
        guest_probe,
        executor_platform_probe,
        http_admitter_probe,
        event_materializer_probe,
        administration_probe,
        scheduler_probe,
        otel_exporter_probe,
    ];

    plan::validate_session_identity(&config)?;
    config.probes = probes.into_boxed_slice();
    Ok(config)
}

/// Reach every configured service once within one finite startup budget.
pub async fn preflight_config(config: &DevConfig) -> Result<(), DevConfigError> {
    let deadline = Instant::now() + STARTUP_REACHABILITY_BUDGET;
    for probe in &config.probes {
        match timeout_at(deadline, TcpStream::connect((&*probe.host, probe.port))).await {
            Ok(Ok(stream)) => drop(stream),
            Ok(Err(source)) => {
                return Err(DevConfigError::endpoint(
                    DevConfigErrorType::EndpointUnreachable,
                    probe.key,
                    probe.sanitized_endpoint.clone(),
                    "endpoint did not accept a connection",
                )
                .with_source(source));
            }
            Err(_) => {
                return Err(DevConfigError::endpoint(
                    DevConfigErrorType::EndpointUnreachable,
                    probe.key,
                    probe.sanitized_endpoint.clone(),
                    "startup reachability budget expired",
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct PackageSource {
    root: PathBuf,
    manifest: PackageManifest,
}

/// Resolve every overlay dependency from explicit package-source candidates.
pub fn resolve_dev_packages(
    config: &DevConfig,
    overlay_root: &Path,
) -> Result<ResolvedDevPackages, DevPackageError> {
    let overlay_manifest = read_package_manifest(overlay_root)?;

    let sources = config
        .package_sources
        .iter()
        .map(|root| {
            read_package_manifest(root).map(|manifest| PackageSource {
                root: root.clone(),
                manifest,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let ignored_package_sources = sources
        .iter()
        .filter(|source| {
            !overlay_manifest
                .base_dependencies
                .values()
                .any(|dependency| package_source_matches(source, dependency))
        })
        .map(|source| source.root.clone())
        .collect::<Vec<_>>()
        .into_boxed_slice();

    let mut base_packages = Vec::with_capacity(overlay_manifest.base_dependencies.len());
    for (alias, dependency) in &overlay_manifest.base_dependencies {
        let matches = sources
            .iter()
            .filter(|source| package_source_matches(source, dependency))
            .collect::<Vec<_>>();
        let coordinate = format!("{}@{}", dependency.package, dependency.version);
        let source = match matches.as_slice() {
            [] => {
                return Err(DevPackageError::dependency(
                    DevPackageErrorType::BaseDependencyMissing,
                    coordinate,
                    dependency.digest.as_str(),
                    &config.package_sources,
                ));
            }
            [source] => *source,
            _ => {
                return Err(DevPackageError::dependency(
                    DevPackageErrorType::BaseDependencyAmbiguous,
                    coordinate,
                    dependency.digest.as_str(),
                    &config.package_sources,
                ));
            }
        };
        base_packages.push(ResolvedBasePackage {
            alias: alias.clone().into_boxed_str(),
            root: source.root.clone(),
            manifest: source.manifest.clone(),
            component_digest: BaseComponentDigestExpectation {
                coordinate: format!("{}@{}", dependency.package, dependency.version)
                    .into_boxed_str(),
                expected: dependency.digest.clone().into_boxed_str(),
            },
        });
    }

    Ok(ResolvedDevPackages {
        overlay_root: overlay_root.to_path_buf(),
        overlay_manifest,
        base_packages: base_packages.into_boxed_slice(),
        ignored_package_sources,
    })
}

pub(crate) fn read_package_manifest(root: &Path) -> Result<PackageManifest, DevPackageError> {
    if wamn_compiler::is_authored(root) {
        let path = root.join(wamn_compiler::AUTHORED_MANIFEST);
        let bytes = wamn_compiler::compile_manifest(root).map_err(|source| {
            DevPackageError::manifest(DevPackageErrorType::ManifestInvalid, path.clone(), source)
        })?;
        return PackageManifest::from_slice(&bytes).map_err(|source| {
            DevPackageError::manifest(DevPackageErrorType::ManifestInvalid, path, source)
        });
    }
    // JSON in a build output directory still consumes an artifact.
    let path = wamn_compiler::package_manifest_path(root);
    if path.parent() == Some(wamn_compiler::output_root(root).as_path()) {
        wamn_compiler::build_record::require_current(root).map_err(|source| {
            DevPackageError::manifest(
                DevPackageErrorType::ManifestInvalid,
                root.join(wamn_compiler::build_record::BUILD_RECORD_FILE),
                source,
            )
        })?;
    }
    read_manifest_file(&path)
}

fn read_manifest_file(path: &Path) -> Result<PackageManifest, DevPackageError> {
    let bytes = fs::read(path).map_err(|source| {
        DevPackageError::manifest(
            DevPackageErrorType::ManifestRead,
            path.to_path_buf(),
            source,
        )
    })?;
    PackageManifest::from_slice(&bytes).map_err(|source| {
        DevPackageError::manifest(
            DevPackageErrorType::ManifestInvalid,
            path.to_path_buf(),
            source,
        )
    })
}

fn database_probe(
    key: &'static str,
    raw: &str,
) -> Result<(ReachabilityProbe, DatabaseIdentity), DevConfigError> {
    let parsed = parse_url(key, raw, &["postgres", "postgresql"])?;
    let probe = probe_from_url(key, &parsed, 5432, true)?;
    if parsed
        .query_pairs()
        .any(|(name, _)| POSTGRES_ROUTING_QUERY_KEYS.contains(&name.as_ref()))
    {
        return Err(DevConfigError::endpoint(
            DevConfigErrorType::InvalidValue,
            key,
            probe.sanitized_endpoint,
            "remove host, hostaddr, port, dbname, and user query overrides; use the URL authority and path",
        ));
    }
    let database_path = parsed.path().strip_prefix('/').unwrap_or_default();
    if database_path.is_empty() || database_path.contains('/') {
        return Err(DevConfigError::endpoint(
            DevConfigErrorType::InvalidValue,
            key,
            probe.sanitized_endpoint,
            "expected one explicit database name",
        ));
    }
    let postgres = PostgresConfig::from_str(raw).map_err(|_| {
        DevConfigError::endpoint(
            DevConfigErrorType::InvalidValue,
            key,
            probe.sanitized_endpoint.clone(),
            "expected a PostgreSQL connection URL",
        )
    })?;
    let database = postgres.get_dbname().unwrap_or_default();
    if database.is_empty() || database.contains('/') {
        return Err(DevConfigError::endpoint(
            DevConfigErrorType::InvalidValue,
            key,
            probe.sanitized_endpoint,
            "expected one explicit database name",
        ));
    }
    Identifier::new(database).map_err(|_| {
        DevConfigError::endpoint(
            DevConfigErrorType::InvalidValue,
            key,
            probe.sanitized_endpoint.clone(),
            "set an explicit database name of at most 63 bytes without NUL",
        )
    })?;
    let user = postgres.get_user().unwrap_or_default();
    if user.is_empty() {
        return Err(DevConfigError::endpoint(
            DevConfigErrorType::InvalidValue,
            key,
            probe.sanitized_endpoint,
            "expected one explicit database role",
        ));
    }
    let identity = DatabaseIdentity {
        host: probe.host.clone(),
        port: probe.port,
        database: database.into(),
        user: user.into(),
    };
    Ok((probe, identity))
}

use plan::validate_disposable_target_database;

use plan::validate_runtime_database_credentials;

use plan::url_probe;

use plan::parse_url;

use plan::probe_from_url;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;
    use tokio::net::TcpListener;

    const ENDPOINT_COUNT: usize = 10;
    /// The overlay manifest of the platform fixture application.
    fn overlay_manifest() -> Vec<u8> {
        wamn_compiler::build_record::require_current(&wamn_fixture_package::overlay_root())
            .unwrap_or_else(|error| panic!("{error:#}"));
        wamn_fixture_package::overlay_manifest_bytes()
    }

    struct TempPackage {
        root: PathBuf,
    }

    impl TempPackage {
        fn with_manifest(bytes: &[u8]) -> Self {
            static NEXT_ID: AtomicU64 = AtomicU64::new(0);

            let root = std::env::temp_dir().join(format!(
                "wamn-dev-package-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).expect("create isolated package fixture");
            fs::write(root.join(PACKAGE_MANIFEST_FILE), bytes)
                .expect("write strict package manifest fixture");
            Self { root }
        }

        fn root(&self) -> &Path {
            &self.root
        }
    }

    impl Drop for TempPackage {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).expect("remove isolated package fixture");
        }
    }

    pub(crate) fn complete_document(addresses: &[SocketAddr; ENDPOINT_COUNT]) -> Value {
        json!({
            (TARGET_DATABASE_URL): format!("postgresql://target:target-secret@{}/target", addresses[0]),
            (TARGET_PRIVILEGES_FILE): "/run/wamn-dev/privileges.sql",
            (TARGET_TEMPLATE_DATABASE): "target--template",
            (TARGET_DATABASE_ACL_FILE): "/run/wamn-dev/database-acl.sql",
            (SYSTEM_DATABASE_URL): format!("postgresql://system:system-secret@{}/system", addresses[1]),
            (IDENTITY_DATABASE_URL): format!("postgresql://identity:identity-secret@{}/system", addresses[2]),
            (GUEST_DATABASE_URL): format!("postgresql://guest:guest-secret@{}/target", addresses[3]),
            (EXECUTOR_PLATFORM_DATABASE_URL): format!("postgresql://platform:platform-secret@{}/target", addresses[4]),
            (HTTP_ADMITTER_DATABASE_URL): format!("postgresql://admitter:admitter-secret@{}/target", addresses[5]),
            (EVENT_MATERIALIZER_DATABASE_URL): format!("postgresql://materializer:materializer-secret@{}/target", addresses[6]),
            (ADMINISTRATION_DATABASE_URL): format!("postgresql://administration:administration-secret@{}/target", addresses[9]),
            (SCHEDULER_NATS_URL): format!("nats://{}", addresses[7]),
            (NATS_TLS_CA): "/run/wamn-dev/nats/ca.crt",
            (NATS_TLS_CERT): "/run/wamn-dev/nats/client.crt",
            (NATS_TLS_KEY): "/run/wamn-dev/nats/client.key",
            (STREAM_REPLICAS): 1,
            (DUP_WINDOW_SECS): 120,
            (OTEL_EXPORTER_OTLP_ENDPOINT): format!("http://{}", addresses[8]),
            (CONDITION_BEARER_TOKEN): "condition-super-secret",
            (ROUTE_HOST): "fixture.localhost",
            (PLATFORM_DOMAIN): "example.invalid",
            (LOCAL_ARTIFACTS): {"directory": "/tmp/wamn-local-candidate",
                "flow_http_component": "/tmp/wamn-flow-http.wasm",
                "materializer_component": "/tmp/wamn-materializer.wasm"},
            (PACKAGE_SOURCES): [],
            (CATALOG): "default",
            (ENVIRONMENT): "fixture-dev",
            (ORG): "acme",
            (PROJECT): "fixture",
            (HOST_GROUP): "wamn-dev-fixture",
            (HOST_NAME): "wamn-dev-fixture-1",
            (RUNNER): "wamn-dev-fixture-1",
            (HOST_BINARY): "/opt/wamn/bin/wamn-host",
            (WASMTIME_CACHE_DIR): "/tmp/wamn-dev-cache",
            (HOST_CONFIG_FILE): "/tmp/wamn-dev/host-config.yaml",
        })
    }

    async fn listener() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind semantic endpoint");
        let address = listener.local_addr().expect("read semantic endpoint");
        let accepted = tokio::spawn(async move {
            listener.accept().await.expect("accept preflight probe");
        });
        (address, accepted)
    }

    #[test]
    fn event_configuration_requires_credentials_and_explicit_stream_limits() {
        let addresses = ["127.0.0.1:41000".parse().expect("test address"); ENDPOINT_COUNT];
        let document = complete_document(&addresses);
        let parsed = parse_config(&serde_json::to_vec(&document).unwrap()).unwrap();
        assert_eq!(
            parsed.nats().nats_tls_cert,
            Path::new("/run/wamn-dev/nats/client.crt")
        );
        assert_eq!(parsed.stream_replicas(), 1);
        assert_eq!(parsed.dup_window_secs(), 120);
        let mut long_window = document.clone();
        long_window[DUP_WINDOW_SECS] = json!(u64::from(u32::MAX) + 1);
        let parsed = parse_config(&serde_json::to_vec(&long_window).unwrap()).unwrap();
        assert_eq!(parsed.dup_window_secs(), u64::from(u32::MAX) + 1);
        for (key, value) in [
            (NATS_TLS_CERT, json!("relative/client.crt")),
            (NATS_TLS_KEY, json!("")),
            (STREAM_REPLICAS, json!(0)),
            (STREAM_REPLICAS, json!(6)),
            (DUP_WINDOW_SECS, json!(0)),
        ] {
            let mut invalid = document.clone();
            invalid[key] = value;
            let error = parse_config(&serde_json::to_vec(&invalid).unwrap()).unwrap_err();
            assert_eq!(error.key(), key);
            assert_eq!(error.error_type(), DevConfigErrorType::InvalidValue);
        }
        for key in [
            NATS_TLS_CA,
            NATS_TLS_CERT,
            NATS_TLS_KEY,
            STREAM_REPLICAS,
            DUP_WINDOW_SECS,
        ] {
            let mut missing = document.clone();
            missing.as_object_mut().unwrap().remove(key);
            let error = parse_config(&serde_json::to_vec(&missing).unwrap()).unwrap_err();
            assert_eq!(error.key(), key);
            assert_eq!(error.error_type(), DevConfigErrorType::MissingKey);
        }
    }

    #[test]
    fn local_artifacts_require_absolute_paths_and_keep_authority_inputs() {
        let addresses = ["127.0.0.1:41000".parse().unwrap(); ENDPOINT_COUNT];
        let mut document = complete_document(&addresses);
        let config = parse_config(&serde_json::to_vec(&document).unwrap()).unwrap();
        assert_eq!(
            config.local_artifacts().directory,
            Path::new("/tmp/wamn-local-candidate")
        );
        assert!(
            config
                .probes
                .iter()
                .any(|probe| probe.key == IDENTITY_DATABASE_URL)
        );
        document["local_artifacts"]["directory"] = json!("relative-directory");
        assert_eq!(
            parse_config(&serde_json::to_vec(&document).unwrap())
                .unwrap_err()
                .key(),
            "local_artifacts"
        );
        document["local_artifacts"]["directory"] = json!("/tmp/wamn-local-candidate");
        document["local_artifacts"]["unexpected"] = json!(true);
        assert_eq!(
            parse_config(&serde_json::to_vec(&document).unwrap())
                .unwrap_err()
                .key(),
            "local_artifacts"
        );
    }

    #[test]
    fn generated_schema_and_strict_parser_share_one_field_authority() {
        let first = dev_config_schema_bytes();
        let second = dev_config_schema_bytes();
        assert_eq!(first, second);
        assert_eq!(first.last(), Some(&b'\n'));

        let schema: Value = serde_json::from_slice(&first).expect("parse generated schema");
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(schema["properties"][PACKAGE_SOURCES]["type"], "array");
        assert_eq!(
            schema["properties"][PACKAGE_SOURCES]["items"]["type"],
            "string"
        );
        let required = schema["required"].as_array().expect("schema required set");
        for expected in [PACKAGE_SOURCES, OTEL_EXPORTER_OTLP_ENDPOINT] {
            assert!(required.iter().any(|key| key == expected));
        }

        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        let mut missing = complete_document(&addresses);
        missing
            .as_object_mut()
            .expect("fixture object")
            .remove(PACKAGE_SOURCES);
        let error = parse_config(&serde_json::to_vec(&missing).expect("serialize missing key"))
            .expect_err("package_sources is required");
        assert_eq!(error.error_type(), DevConfigErrorType::MissingKey);
        assert_eq!(error.key(), PACKAGE_SOURCES);

        let mut malformed = complete_document(&addresses);
        malformed[PACKAGE_SOURCES] = json!(["/valid", 7]);
        let error = parse_config(
            &serde_json::to_vec(&malformed).expect("serialize malformed package sources"),
        )
        .expect_err("package source entries must be paths");
        assert_eq!(error.error_type(), DevConfigErrorType::InvalidValue);
        assert_eq!(error.key(), PACKAGE_SOURCES);

        let config = parse_config(
            &serde_json::to_vec(&complete_document(&addresses)).expect("serialize complete config"),
        )
        .expect("complete config parses");
        assert_eq!(config.host_binary(), Path::new("/opt/wamn/bin/wamn-host"));
        assert_eq!(
            config.wasmtime_cache_dir(),
            Path::new("/tmp/wamn-dev-cache")
        );
        assert_eq!(config.platform_domain(), "example.invalid");
        assert_eq!(
            config.otel_exporter_otlp_endpoint(),
            format!("http://{}", addresses[8])
        );

        for key in [PLATFORM_DOMAIN, HOST_BINARY, WASMTIME_CACHE_DIR] {
            let mut missing = complete_document(&addresses);
            missing.as_object_mut().expect("fixture object").remove(key);
            let error = parse_config(
                &serde_json::to_vec(&missing).expect("serialize missing required input"),
            )
            .expect_err("every identity and local-host input is required");
            assert_eq!(error.error_type(), DevConfigErrorType::MissingKey);
            assert_eq!(error.key(), key);
        }

        let mut invalid_domain = complete_document(&addresses);
        invalid_domain[PLATFORM_DOMAIN] = json!("example.invalid/path");
        let error = parse_config(
            &serde_json::to_vec(&invalid_domain).expect("serialize invalid platform domain"),
        )
        .expect_err("the platform domain must be a domain name");
        assert_eq!(error.error_type(), DevConfigErrorType::InvalidValue);
        assert_eq!(error.key(), PLATFORM_DOMAIN);
    }

    #[test]
    fn source_resolution_accepts_absent_and_stale_build_records() {
        use wamn_compiler::build_record;

        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
        let root = scratch.path().join("source_package");
        fs::create_dir(&root).unwrap();
        let authored = r#"import manifest
manifest.Package {
    package = {id = "source_package", version = "1.0.0"}
    required_platform_policy_contract = {id = "source_data_access", state = "satisfied"}
    models = {
        item = manifest.Model("item", "source_data") {
            table = "item"
            owner = "source_package"
            audit_log = {columns = ["created_at"]}
            operations: {get: {result = "one"}}
        }
    }
    components = {source = {connections = ["postgres"]}}
}
"#;
        fs::write(root.join("wamn.k"), authored).unwrap();
        let addresses = ["127.0.0.1:41000".parse().unwrap(); ENDPOINT_COUNT];
        let config =
            parse_config(&serde_json::to_vec(&complete_document(&addresses)).unwrap()).unwrap();
        let output = wamn_compiler::output_root(&root);

        let resolved = resolve_dev_packages(&config, &root).unwrap();
        assert_eq!(resolved.overlay_manifest().package.version, "1.0.0");
        assert!(!output.exists(), "source resolution writes no build output");

        let built: build_record::BuildRecord = serde_json::from_value(json!({
            "schema": build_record::BUILD_RECORD_SCHEMA,
            "package": {"id": "source_package", "version": "1.0.0"},
            "inputs": {"digest": build_record::input_digest(&root).unwrap(), "generation_digest": build_record::generation_digest(&root).unwrap(), "files": {}},
            "dependencies": [],
            "generator": {"id": "generator", "toolchain": "toolchain", "sqlx": "0.9.0"},
            "outputs": {"manifest": "sha256:fixture", "files": {}, "components": []}
        }))
        .unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(
            output.join(build_record::BUILD_RECORD_FILE),
            built.to_bytes(),
        )
        .unwrap();
        fs::write(output.join("wamn.json"), b"previous compiled output").unwrap();
        fs::write(root.join("wamn.k"), authored.replace("1.0.0", "1.0.1")).unwrap();
        fs::create_dir(root.join("migrations")).unwrap();
        fs::write(root.join("migrations/0001_initial.sql"), "SELECT 1;\n").unwrap();
        build_record::require_current(&root).unwrap_err();

        let resolved = resolve_dev_packages(&config, &root).unwrap();
        assert_eq!(resolved.overlay_manifest().package.version, "1.0.1");
        assert_eq!(
            fs::read(output.join("wamn.json")).unwrap(),
            b"previous compiled output"
        );
        assert_eq!(
            fs::read(output.join(build_record::BUILD_RECORD_FILE)).unwrap(),
            built.to_bytes()
        );
        wamn_package_control::component_admission::load_package_statement_facts(
            &root,
            resolved.overlay_manifest(),
        )
        .unwrap_err();
    }

    #[test]
    fn overlay_dependencies_resolve_by_coordinate_and_report_ignored_sources() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        // Resolution must not overwrite the live fixture's derived manifest.
        let overlay = TempPackage::with_manifest(&overlay_manifest());
        let base =
            TempPackage::with_manifest(&wamn_package_control::test_support::fixture_manifest());
        let overlay_root = overlay.root().to_path_buf();
        let base_root = base.root().to_path_buf();
        let ignored_root = overlay_root.clone();
        let mut document = complete_document(&addresses);
        document[PACKAGE_SOURCES] = json!([ignored_root, base_root]);
        let config = parse_config(&serde_json::to_vec(&document).expect("serialize config"))
            .expect("package-source config parses");

        let resolved = resolve_dev_packages(&config, &overlay_root)
            .expect("the exact manifest-declared base resolves");

        assert_eq!(resolved.overlay_root(), overlay_root);
        assert_eq!(
            resolved.overlay_manifest().package.id,
            wamn_fixture_package::OVERLAY_PACKAGE_ID
        );
        assert_eq!(resolved.base_packages().len(), 1);
        assert_eq!(resolved.ignored_package_source_count(), 1);
        assert_eq!(resolved.ignored_package_sources(), [overlay_root]);
        let base = &resolved.base_packages()[0];
        assert_eq!(base.alias(), "base_fixture");
        assert_eq!(base.root(), base_root);
        assert_eq!(base.manifest().package.id, wamn_fixture_package::PACKAGE_ID);
        assert_eq!(
            base.component_digest().expected(),
            resolved.overlay_manifest().base_dependencies["base_fixture"].digest
        );

        let verified = base
            .component_digest()
            .verify(base.component_digest().expected());
        assert_eq!(verified.coordinate(), "platform_fixture@2.1.1");
        assert_eq!(verified.digest(), base.component_digest().expected());
        assert_eq!(verified.superseded_pin(), None);
    }

    fn moved_digest_expectation() -> (BaseComponentDigestExpectation, String) {
        let expectation = BaseComponentDigestExpectation {
            coordinate: "platform_fixture@2.1.0".into(),
            expected: format!("sha256:{}", "a".repeat(64)).into_boxed_str(),
        };
        let observed = format!("sha256:{}", "b".repeat(64));
        (expectation, observed)
    }

    #[test]
    fn a_moved_digest_carries_the_observed_digest_and_records_the_drift() {
        let (expectation, observed) = moved_digest_expectation();

        let verified = expectation.verify(observed.as_str());
        assert_eq!(verified.coordinate(), expectation.coordinate());
        assert_eq!(
            verified.digest(),
            observed.as_str(),
            "later stages must carry the digest that was built, never the stale pin"
        );
        assert_eq!(verified.superseded_pin(), Some(expectation.expected()));
    }

    #[test]
    fn an_unmoved_digest_records_no_drift() {
        let (expectation, _) = moved_digest_expectation();

        let verified = expectation.verify(expectation.expected());
        assert_eq!(verified.digest(), expectation.expected());
        assert_eq!(verified.superseded_pin(), None);
    }

    #[test]
    fn missing_and_ambiguous_dependencies_name_the_complete_search() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        // Resolution must not overwrite the live fixture's derived manifest.
        let overlay = TempPackage::with_manifest(&overlay_manifest());
        let base =
            TempPackage::with_manifest(&wamn_package_control::test_support::fixture_manifest());
        let overlay_root = overlay.root().to_path_buf();
        let base_root = base.root().to_path_buf();
        let expected = wamn_compiler::manifest::PackageManifest::from_slice(&overlay_manifest())
            .expect("parse repository overlay")
            .base_dependencies
            .remove("base_fixture")
            .expect("overlay declares its base");

        let mut missing_document = complete_document(&addresses);
        missing_document[PACKAGE_SOURCES] = json!([overlay_root]);
        let missing_config = parse_config(
            &serde_json::to_vec(&missing_document).expect("serialize missing-source config"),
        )
        .expect("missing source is a resolution concern");
        let error = resolve_dev_packages(&missing_config, &overlay_root)
            .expect_err("zero coordinate matches must refuse");
        assert_eq!(
            error.error_type(),
            DevPackageErrorType::BaseDependencyMissing
        );
        assert_eq!(error.coordinate(), Some("platform_fixture@2.1.1"));
        assert_eq!(error.dependency_digest(), Some(expected.digest.as_str()));
        assert_eq!(error.searched_roots(), std::slice::from_ref(&overlay_root));

        let mut ambiguous_document = complete_document(&addresses);
        ambiguous_document[PACKAGE_SOURCES] = json!([base_root.clone(), overlay_root, base_root]);
        let ambiguous_config = parse_config(
            &serde_json::to_vec(&ambiguous_document).expect("serialize ambiguous-source config"),
        )
        .expect("duplicate coordinates are a resolution concern");
        let error = resolve_dev_packages(&ambiguous_config, &overlay_root)
            .expect_err("multiple coordinate matches must refuse");
        assert_eq!(
            error.error_type(),
            DevPackageErrorType::BaseDependencyAmbiguous
        );
        assert_eq!(error.coordinate(), Some("platform_fixture@2.1.1"));
        assert_eq!(error.dependency_digest(), Some(expected.digest.as_str()));
        assert_eq!(
            error.searched_roots(),
            [base_root.clone(), overlay_root, base_root]
        );
    }

    #[test]
    fn overlay_manifest_is_parsed_through_the_strict_package_parser() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        let mut overlay: Value =
            serde_json::from_slice(&overlay_manifest()).expect("parse overlay fixture");
        overlay["environment_url"] = json!("https://must-not-enter-a-package.invalid");
        let overlay = TempPackage::with_manifest(
            &serde_json::to_vec(&overlay).expect("serialize invalid overlay"),
        );
        let config = parse_config(
            &serde_json::to_vec(&complete_document(&addresses)).expect("serialize config"),
        )
        .expect("config parses");

        let error = resolve_dev_packages(&config, overlay.root())
            .expect_err("unknown package fields must refuse");

        assert_eq!(error.error_type(), DevPackageErrorType::ManifestInvalid);
        assert_eq!(
            error.manifest_path(),
            Some(overlay.root().join(PACKAGE_MANIFEST_FILE).as_path())
        );
    }

    #[tokio::test]
    async fn complete_config_reaches_every_declared_endpoint_once() {
        let mut addresses = Vec::with_capacity(ENDPOINT_COUNT);
        let mut accepted = Vec::with_capacity(ENDPOINT_COUNT);
        for _ in 0..ENDPOINT_COUNT {
            let (address, task) = listener().await;
            addresses.push(address);
            accepted.push(task);
        }
        let addresses: [SocketAddr; ENDPOINT_COUNT] = addresses
            .try_into()
            .expect("all endpoint addresses are present");
        let bytes = serde_json::to_vec(&complete_document(&addresses)).expect("serialize fixture");

        let config = parse_config(&bytes).expect("complete strict config parses");
        preflight_config(&config)
            .await
            .expect("every semantic endpoint is reachable");

        for task in accepted {
            task.await.expect("join semantic endpoint");
        }
        let debug = format!("{config:?}");
        for credential in [
            "target-secret",
            "system-secret",
            "identity-secret",
            "guest-secret",
            "platform-secret",
            "admitter-secret",
            "materializer-secret",
            "administration-secret",
            "condition-super-secret",
        ] {
            assert!(!debug.contains(credential), "Debug leaked {credential}");
        }
    }

    #[tokio::test]
    async fn unreachable_endpoint_names_only_its_key_and_sanitized_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("reserve refused endpoint");
        let address = listener.local_addr().expect("read refused endpoint");
        drop(listener);
        let addresses = [address; ENDPOINT_COUNT];
        let bytes = serde_json::to_vec(&complete_document(&addresses)).expect("serialize fixture");
        let config = parse_config(&bytes).expect("endpoint syntax is valid");

        let error = tokio::time::timeout(Duration::from_secs(1), preflight_config(&config))
            .await
            .expect("local refusal stays within the startup bound")
            .expect_err("closed endpoint must refuse");

        assert_eq!(error.error_type(), DevConfigErrorType::EndpointUnreachable);
        assert_eq!(error.key(), TARGET_DATABASE_URL);
        assert_eq!(
            error.sanitized_endpoint(),
            Some(format!("postgresql://{address}/target").as_str())
        );
        let message = error.to_string();
        assert!(!message.contains("target-secret"));
        assert!(!message.contains("condition-super-secret"));
    }

    #[test]
    fn malformed_unknown_and_missing_inputs_refuse_at_their_exact_key() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        let mut malformed = complete_document(&addresses);
        malformed[TARGET_DATABASE_URL] = json!("postgresql://user:secret@[");
        let error = parse_config(&serde_json::to_vec(&malformed).expect("serialize malformed"))
            .expect_err("malformed endpoint must refuse");
        assert_eq!(error.error_type(), DevConfigErrorType::InvalidValue);
        assert_eq!(error.key(), TARGET_DATABASE_URL);
        assert_eq!(error.sanitized_endpoint(), Some("<malformed>"));
        assert!(!error.to_string().contains("secret"));

        let mut unknown = complete_document(&addresses);
        unknown["project_database_url"] = json!("postgresql://ignored:secret@invalid/db");
        let error = parse_config(&serde_json::to_vec(&unknown).expect("serialize unknown"))
            .expect_err("unknown key must refuse");
        assert_eq!(error.error_type(), DevConfigErrorType::UnknownKey);
        assert_eq!(error.key(), "project_database_url");
        assert!(!error.to_string().contains("ignored"));

        let mut missing_endpoint = complete_document(&addresses);
        missing_endpoint
            .as_object_mut()
            .expect("fixture object")
            .remove(SCHEDULER_NATS_URL);
        let error = parse_config(
            &serde_json::to_vec(&missing_endpoint).expect("serialize missing endpoint"),
        )
        .expect_err("missing endpoint must refuse");
        assert_eq!(error.error_type(), DevConfigErrorType::MissingKey);
        assert_eq!(error.key(), SCHEDULER_NATS_URL);

        {
            let key = OTEL_EXPORTER_OTLP_ENDPOINT;
            let mut missing_observability_endpoint = complete_document(&addresses);
            missing_observability_endpoint
                .as_object_mut()
                .expect("fixture object")
                .remove(key);
            let error = parse_config(
                &serde_json::to_vec(&missing_observability_endpoint)
                    .expect("serialize missing observability endpoint"),
            )
            .expect_err("observability endpoints are required");
            assert_eq!(error.error_type(), DevConfigErrorType::MissingKey);
            assert_eq!(error.key(), key);
        }

        let mut missing_local_artifacts = complete_document(&addresses);
        missing_local_artifacts
            .as_object_mut()
            .expect("fixture object")
            .remove(LOCAL_ARTIFACTS);
        let error = parse_config(
            &serde_json::to_vec(&missing_local_artifacts).expect("serialize missing local input"),
        )
        .expect_err("missing local artifacts must refuse");
        assert_eq!(error.error_type(), DevConfigErrorType::MissingKey);
        assert_eq!(error.key(), LOCAL_ARTIFACTS);

        let mut missing_runtime_role = complete_document(&addresses);
        missing_runtime_role
            .as_object_mut()
            .expect("fixture object")
            .remove(EVENT_MATERIALIZER_DATABASE_URL);
        let error = parse_config(
            &serde_json::to_vec(&missing_runtime_role).expect("serialize missing runtime role"),
        )
        .expect_err("missing role-exact credential must refuse");
        assert_eq!(error.error_type(), DevConfigErrorType::MissingKey);
        assert_eq!(error.key(), EVENT_MATERIALIZER_DATABASE_URL);

        let mut tls_scheduler = complete_document(&addresses);
        tls_scheduler[SCHEDULER_NATS_URL] = json!("tls://scheduler.invalid:4222");
        let error = parse_config(
            &serde_json::to_vec(&tls_scheduler).expect("serialize unsupported TLS endpoint"),
        )
        .expect_err("TLS without trust configuration must refuse");
        assert_eq!(error.error_type(), DevConfigErrorType::InvalidValue);
        assert_eq!(error.key(), SCHEDULER_NATS_URL);

        {
            let key = OTEL_EXPORTER_OTLP_ENDPOINT;
            let mut unsupported_observability_endpoint = complete_document(&addresses);
            unsupported_observability_endpoint[key] = json!("nats://127.0.0.1:41000");
            let error = parse_config(
                &serde_json::to_vec(&unsupported_observability_endpoint)
                    .expect("serialize unsupported observability endpoint"),
            )
            .expect_err("observability endpoints require HTTP or HTTPS");
            assert_eq!(error.error_type(), DevConfigErrorType::InvalidValue);
            assert_eq!(error.key(), key);
        }
    }

    #[test]
    fn postgres_identity_routing_query_overrides_refuse_without_leaking_values() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        for query_key in POSTGRES_ROUTING_QUERY_KEYS {
            let mut document = complete_document(&addresses);
            document[TARGET_DATABASE_URL] = json!(format!(
                "postgresql://target:target-secret@127.0.0.1:41000/target?{query_key}=override-secret"
            ));

            let error =
                parse_config(&serde_json::to_vec(&document).expect("serialize routing override"))
                    .expect_err("routing query override must refuse");

            assert_eq!(error.error_type(), DevConfigErrorType::InvalidValue);
            assert_eq!(error.key(), TARGET_DATABASE_URL);
            assert!(error.to_string().contains("remove host, hostaddr, port"));
            assert!(!error.to_string().contains("override-secret"));
            assert!(!error.to_string().contains("target-secret"));
        }
    }

    #[test]
    fn runtime_database_roles_refuse_privileged_or_sibling_credentials() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];

        let mut privileged_reuse = complete_document(&addresses);
        let target_credential = privileged_reuse[TARGET_DATABASE_URL].clone();
        privileged_reuse[GUEST_DATABASE_URL] = target_credential;
        let error = parse_config(
            &serde_json::to_vec(&privileged_reuse).expect("serialize privileged reuse"),
        )
        .expect_err("a runtime role must not reuse the target credential");
        assert_eq!(error.error_type(), DevConfigErrorType::DatabaseCollision);
        assert_eq!(error.key(), GUEST_DATABASE_URL);
        assert!(!error.to_string().contains("target-secret"));

        let mut sibling_reuse = complete_document(&addresses);
        let platform_credential = sibling_reuse[EXECUTOR_PLATFORM_DATABASE_URL].clone();
        sibling_reuse[EVENT_MATERIALIZER_DATABASE_URL] = platform_credential;
        let error =
            parse_config(&serde_json::to_vec(&sibling_reuse).expect("serialize sibling reuse"))
                .expect_err("runtime roles must not share one credential");
        assert_eq!(error.error_type(), DevConfigErrorType::DatabaseCollision);
        assert_eq!(error.key(), EVENT_MATERIALIZER_DATABASE_URL);
        assert!(!error.to_string().contains("platform-secret"));
    }

    #[test]
    fn malformed_json_refuses_without_inspecting_partial_credentials() {
        let error = parse_config(br#"{"condition_bearer_token":"secret""#)
            .expect_err("malformed JSON must refuse");
        assert_eq!(error.error_type(), DevConfigErrorType::MalformedDocument);
        assert_eq!(error.key(), DOCUMENT_KEY);
        assert!(!error.to_string().contains("secret"));
    }
}
