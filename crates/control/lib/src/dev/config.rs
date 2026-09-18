//! Strict deployment-owned configuration for the development loop.
//!
//! This module validates and preflights externally supplied services. It does
//! not provision them or execute any development stage.

use std::error::Error;
use std::fmt;
use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout_at};
use tokio_postgres::Config as PostgresConfig;
use url::Url;
use wamn_pg_core::Identifier;
use wamn_schema_generator::{PackageManifest, validate_operation_vocabulary};

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
const SCHEDULER_NATS_URL: &str = "scheduler_nats_url";
const EVENT_NATS_URL: &str = "event_nats_url";
const EVENT_NATS_USERNAME: &str = "event_nats_username";
const EVENT_NATS_PASSWORD_FILE: &str = "event_nats_password_file";
const STREAM_REPLICAS: &str = "stream_replicas";
const DUP_WINDOW_SECS: &str = "dup_window_secs";
const TEMPO_QUERY_URL: &str = "tempo_query_url";
const OTEL_EXPORTER_OTLP_ENDPOINT: &str = "otel_exporter_otlp_endpoint";
const GATE_BEARER_TOKEN: &str = "gate_bearer_token";
const OPERATOR_BEARER_TOKEN: &str = "operator_bearer_token";
const ROUTE_HOST: &str = "route_host";
const PLATFORM_DOMAIN: &str = "platform_domain";
const LOCAL_ARTIFACTS: &str = "local_artifacts";
const PACKAGE_SOURCES: &str = "package_sources";
const EFFECTIVE_RELEASE_ID: &str = "effective_release_id";
const TENANT: &str = "tenant";
const CATALOG: &str = "catalog";
const ENVIRONMENT: &str = "environment";
const ORG: &str = "org";
const PROJECT: &str = "project";
const SCHEMA: &str = "schema";
const HOST_GROUP: &str = "host_group";
const HOST_NAME: &str = "host_name";
const RUNNER: &str = "runner";
const HOST_BINARY: &str = "host_binary";
const WASMTIME_CACHE_DIR: &str = "wasmtime_cache_dir";
const PACKAGE_MANIFEST_FILE: &str = "wamn.json";

pub(super) const POSTGRES_SYSTEM_DATABASES: [&str; 3] = ["postgres", "template0", "template1"];
const POSTGRES_ROUTING_QUERY_KEYS: [&str; 5] = ["host", "hostaddr", "port", "dbname", "user"];

/// Explicit local candidate files owned by one disposable development session.
#[derive(Clone, Debug, Deserialize, serde::Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalArtifacts {
    pub directory: PathBuf,
    pub flow_http_component: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bindings: Option<PathBuf>,
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
    session_identity: Option<SessionIdentity>,
    guest_database_url: String,
    executor_platform_database_url: String,
    http_admitter_database_url: String,
    event_materializer_database_url: String,
    scheduler_nats_url: String,
    event_nats_url: String,
    event_nats_username: String,
    event_nats_password_file: PathBuf,
    stream_replicas: usize,
    dup_window_secs: u64,
    tempo_query_url: String,
    otel_exporter_otlp_endpoint: String,
    gate_bearer_token: String,
    #[serde(default)]
    #[schemars(with = "String")]
    operator_bearer_token: Option<String>,
    route_host: String,
    platform_domain: String,
    local_artifacts: LocalArtifacts,
    package_sources: Vec<PathBuf>,
    effective_release_id: NonZeroU32,
    tenant: String,
    catalog: String,
    environment: String,
    org: String,
    project: String,
    schema: String,
    host_group: String,
    host_name: String,
    runner: String,
    host_binary: PathBuf,
    wasmtime_cache_dir: PathBuf,
}

/// Stable category of a development configuration refusal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevConfigErrorKind {
    MalformedDocument,
    UnknownKey,
    MissingKey,
    InvalidValue,
    DatabaseCollision,
    EndpointUnreachable,
}

impl DevConfigErrorKind {
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
    kind: DevConfigErrorKind,
    key: Box<str>,
    endpoint: Option<Box<str>>,
    detail: &'static str,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl DevConfigError {
    fn new(kind: DevConfigErrorKind, key: impl Into<Box<str>>, detail: &'static str) -> Self {
        Self {
            kind,
            key: key.into(),
            endpoint: None,
            detail,
            source: None,
        }
    }

    fn endpoint(
        kind: DevConfigErrorKind,
        key: &'static str,
        endpoint: impl Into<Box<str>>,
        detail: &'static str,
    ) -> Self {
        Self {
            kind,
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
    pub const fn kind(&self) -> DevConfigErrorKind {
        self.kind
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
            self.kind.as_str(),
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
pub enum DevPackageErrorKind {
    ManifestRead,
    ManifestInvalid,
    BaseDependencyMissing,
    BaseDependencyAmbiguous,
}

impl DevPackageErrorKind {
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
    kind: DevPackageErrorKind,
    manifest_path: Option<PathBuf>,
    coordinate: Option<Box<str>>,
    dependency_digest: Option<Box<str>>,
    searched_roots: Box<[PathBuf]>,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl DevPackageError {
    fn manifest(
        kind: DevPackageErrorKind,
        manifest_path: PathBuf,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            manifest_path: Some(manifest_path),
            coordinate: None,
            dependency_digest: None,
            searched_roots: Box::new([]),
            source: Some(Box::new(source)),
        }
    }

    fn dependency(
        kind: DevPackageErrorKind,
        coordinate: impl Into<Box<str>>,
        dependency_digest: impl Into<Box<str>>,
        searched_roots: &[PathBuf],
    ) -> Self {
        Self {
            kind,
            manifest_path: None,
            coordinate: Some(coordinate.into()),
            dependency_digest: Some(dependency_digest.into()),
            searched_roots: searched_roots.into(),
            source: None,
        }
    }

    /// Stable refusal category.
    pub const fn kind(&self) -> DevPackageErrorKind {
        self.kind
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
        formatter.write_str(self.kind.as_str())?;
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
                self.kind,
                DevPackageErrorKind::BaseDependencyMissing
                    | DevPackageErrorKind::BaseDependencyAmbiguous
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

/// The base component digest one run checked, and the pin it moved off.
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

    /// Component digest the base build is compared with before Gate.
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
    session_identity: Option<SessionIdentity>,
    guest_database_url: Box<str>,
    executor_platform_database_url: Box<str>,
    http_admitter_database_url: Box<str>,
    event_materializer_database_url: Box<str>,
    scheduler_nats_url: Box<str>,
    event_nats_url: Box<str>,
    event_nats_username: Box<str>,
    event_nats_password_file: PathBuf,
    stream_replicas: usize,
    dup_window_secs: u64,
    tempo_query_url: Box<str>,
    otel_exporter_otlp_endpoint: Box<str>,
    local_artifacts: LocalArtifacts,
    gate_bearer_token: Box<str>,
    operator_bearer_token: Option<Box<str>>,
    route_host: Box<str>,
    platform_domain: Box<str>,
    package_sources: Box<[PathBuf]>,
    effective_release_id: NonZeroU32,
    activation_identity: DevActivationIdentity,
    host_binary: PathBuf,
    wasmtime_cache_dir: PathBuf,
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
                SCHEDULER_NATS_URL,
                &self.sanitized_endpoint(SCHEDULER_NATS_URL),
            )
            .field(EVENT_NATS_URL, &self.sanitized_endpoint(EVENT_NATS_URL))
            .field(EVENT_NATS_USERNAME, &self.event_nats_username)
            .field(EVENT_NATS_PASSWORD_FILE, &self.event_nats_password_file)
            .field(STREAM_REPLICAS, &self.stream_replicas)
            .field(DUP_WINDOW_SECS, &self.dup_window_secs)
            .field(TEMPO_QUERY_URL, &self.sanitized_endpoint(TEMPO_QUERY_URL))
            .field(
                OTEL_EXPORTER_OTLP_ENDPOINT,
                &self.sanitized_endpoint(OTEL_EXPORTER_OTLP_ENDPOINT),
            )
            .field(LOCAL_ARTIFACTS, &self.local_artifacts)
            .field(GATE_BEARER_TOKEN, &"[REDACTED]")
            .field(ROUTE_HOST, &self.route_host)
            .field(PLATFORM_DOMAIN, &self.platform_domain)
            .field(PACKAGE_SOURCES, &self.package_sources)
            .field(EFFECTIVE_RELEASE_ID, &self.effective_release_id)
            .field("activation_identity", &self.activation_identity)
            .field(HOST_BINARY, &self.host_binary)
            .field(WASMTIME_CACHE_DIR, &self.wasmtime_cache_dir)
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

    /// Target PostgreSQL URL. In a development loop this database is
    /// disposable: the loop drops and recreates it before every Apply.
    pub fn target_database_url(&self) -> &str {
        &self.target_database_url
    }

    /// Privilege SQL `wamn dev up` emitted for the target database.
    ///
    /// The loop recreates the target database per run, which drops every
    /// per-database privilege with it. Roles are cluster-level and survive, so
    /// this file is what has to be replayed. It is read, never re-derived: a
    /// privilege set the loop computed itself would not be the one the
    /// environment was provisioned with.
    pub fn target_privileges_file(&self) -> &Path {
        &self.target_privileges_file
    }

    /// Pristine template database `wamn dev up` prepared for this target.
    ///
    /// It carries everything a drop destroys except the database-level ACL:
    /// the platform floor, the run plane and the workload grants. A run clones
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

    /// Scheduler NATS endpoint used by the native workload API.
    pub fn scheduler_nats_url(&self) -> &str {
        &self.scheduler_nats_url
    }

    /// Event-plane NATS endpoint passed to the local serving host.
    pub fn event_nats_url(&self) -> &str {
        &self.event_nats_url
    }

    /// Event username shared by local publication and its environment tap view.
    pub fn event_nats_username(&self) -> &str {
        &self.event_nats_username
    }

    /// Private event password file passed to the local serving host.
    pub fn event_nats_password_file(&self) -> &Path {
        &self.event_nats_password_file
    }

    /// Declared NATS stream copies, separate from workload instances.
    pub fn stream_replicas(&self) -> usize {
        self.stream_replicas
    }

    /// Declared source-stream duplicate window in seconds.
    pub fn dup_window_secs(&self) -> u64 {
        self.dup_window_secs
    }

    /// Tempo HTTP query endpoint used by the development read seam.
    pub fn tempo_query_url(&self) -> &str {
        &self.tempo_query_url
    }

    /// OTLP gRPC exporter endpoint passed to the local serving host.
    pub fn otel_exporter_otlp_endpoint(&self) -> &str {
        &self.otel_exporter_otlp_endpoint
    }

    /// Local input for the unpublished candidate.
    pub const fn local_artifacts(&self) -> &LocalArtifacts {
        &self.local_artifacts
    }

    /// Bearer credential presented only to the Gate.
    pub fn gate_bearer_token(&self) -> &str {
        &self.gate_bearer_token
    }

    /// Operator credential for generated clients, separate from Gate authority.
    pub fn operator_bearer_token(&self) -> Option<&str> {
        self.operator_bearer_token.as_deref()
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

    /// Positive deployment-owned identity for the effective release minted by this loop.
    pub const fn effective_release_id(&self) -> u32 {
        self.effective_release_id.get()
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
}

/// Language-neutral JSON Schema generated from the strict `dev.json` input type.
pub fn dev_config_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(DevConfigDocument)).expect("schema serializes")
}

/// Byte-stable pretty JSON Schema generated from the strict `dev.json` input type.
pub fn dev_config_schema_bytes() -> Vec<u8> {
    let mut bytes =
        serde_json::to_vec_pretty(&dev_config_schema()).expect("dev config schema serializes");
    bytes.push(b'\n');
    bytes
}

/// Parse one strict deployment-owned JSON document.
pub fn parse_config(bytes: &[u8]) -> Result<DevConfig, DevConfigError> {
    let document: Value = serde_json::from_slice(bytes).map_err(|source| {
        DevConfigError::new(
            DevConfigErrorKind::MalformedDocument,
            DOCUMENT_KEY,
            "expected one JSON object",
        )
        .with_source(source)
    })?;
    let object = document.as_object().ok_or_else(|| {
        DevConfigError::new(
            DevConfigErrorKind::MalformedDocument,
            DOCUMENT_KEY,
            "expected one JSON object",
        )
    })?;
    validate_config_document_shape(object)?;
    let input: DevConfigDocument = serde_json::from_value(document).map_err(|source| {
        DevConfigError::new(
            DevConfigErrorKind::MalformedDocument,
            DOCUMENT_KEY,
            "document disagrees with its generated schema",
        )
        .with_source(source)
    })?;
    let DevConfigDocument {
        target_database_url,
        target_privileges_file,
        target_template_database,
        target_database_acl_file,
        system_database_url,
        identity_database_url,
        session_identity,
        guest_database_url,
        executor_platform_database_url,
        http_admitter_database_url,
        event_materializer_database_url,
        scheduler_nats_url,
        event_nats_url,
        event_nats_username,
        event_nats_password_file,
        stream_replicas,
        dup_window_secs,
        tempo_query_url,
        otel_exporter_otlp_endpoint,
        local_artifacts,
        gate_bearer_token,
        operator_bearer_token,
        route_host,
        platform_domain,
        package_sources,
        effective_release_id,
        tenant,
        catalog,
        environment,
        org,
        project,
        schema,
        host_group,
        host_name,
        runner,
        host_binary,
        wasmtime_cache_dir,
    } = input;

    let target_database_url = nonempty_string(target_database_url, TARGET_DATABASE_URL)?;
    let system_database_url = nonempty_string(system_database_url, SYSTEM_DATABASE_URL)?;
    let identity_database_url = nonempty_string(identity_database_url, IDENTITY_DATABASE_URL)?;
    let guest_database_url = nonempty_string(guest_database_url, GUEST_DATABASE_URL)?;
    let executor_platform_database_url = nonempty_string(
        executor_platform_database_url,
        EXECUTOR_PLATFORM_DATABASE_URL,
    )?;
    let http_admitter_database_url =
        nonempty_string(http_admitter_database_url, HTTP_ADMITTER_DATABASE_URL)?;
    let event_materializer_database_url = nonempty_string(
        event_materializer_database_url,
        EVENT_MATERIALIZER_DATABASE_URL,
    )?;
    let scheduler_nats_url = nonempty_string(scheduler_nats_url, SCHEDULER_NATS_URL)?;
    let event_nats_url = nonempty_string(event_nats_url, EVENT_NATS_URL)?;
    let event_nats_username = nonempty_string(event_nats_username, EVENT_NATS_USERNAME)?;
    if !event_nats_username
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(DevConfigError::new(
            DevConfigErrorKind::InvalidValue,
            EVENT_NATS_USERNAME,
            "event username must be one broker token",
        ));
    }
    let event_nats_password_file =
        nonempty_path(event_nats_password_file, EVENT_NATS_PASSWORD_FILE)?;
    if !(1..=5).contains(&stream_replicas) {
        return Err(DevConfigError::new(
            DevConfigErrorKind::InvalidValue,
            STREAM_REPLICAS,
            "NATS stream copies must be between one and five",
        ));
    }
    if dup_window_secs == 0 {
        return Err(DevConfigError::new(
            DevConfigErrorKind::InvalidValue,
            DUP_WINDOW_SECS,
            "source-stream duplicate window must be positive",
        ));
    }
    let tempo_query_url = nonempty_string(tempo_query_url, TEMPO_QUERY_URL)?;
    let otel_exporter_otlp_endpoint =
        nonempty_string(otel_exporter_otlp_endpoint, OTEL_EXPORTER_OTLP_ENDPOINT)?;
    for path in [
        &local_artifacts.directory,
        &local_artifacts.flow_http_component,
    ]
    .into_iter()
    .chain(local_artifacts.bindings.iter())
    {
        if !path.is_absolute() {
            return Err(DevConfigError::new(
                DevConfigErrorKind::InvalidValue,
                LOCAL_ARTIFACTS,
                "local artifact paths must be absolute",
            ));
        }
    }
    let target_privileges_file = nonempty_path(target_privileges_file, TARGET_PRIVILEGES_FILE)?;
    let target_template_database =
        nonempty_string(target_template_database, TARGET_TEMPLATE_DATABASE)?;
    let target_database_acl_file =
        nonempty_path(target_database_acl_file, TARGET_DATABASE_ACL_FILE)?;
    let gate_bearer_token = nonempty_string(gate_bearer_token, GATE_BEARER_TOKEN)?;
    let operator_bearer_token = operator_bearer_token
        .map(|token| nonempty_string(token, OPERATOR_BEARER_TOKEN))
        .transpose()?;
    let route_host = nonempty_string(route_host, ROUTE_HOST)?;
    wamn_control_provision::validate_platform_domain(&platform_domain).map_err(|source| {
        DevConfigError::new(
            DevConfigErrorKind::InvalidValue,
            PLATFORM_DOMAIN,
            "expected a domain name",
        )
        .with_source(source)
    })?;
    let package_sources = package_sources
        .into_iter()
        .map(|root| nonempty_path(root, PACKAGE_SOURCES))
        .collect::<Result<Vec<_>, _>>()?
        .into_boxed_slice();
    for (key, value) in [
        (TENANT, tenant.as_str()),
        (CATALOG, catalog.as_str()),
        (ENVIRONMENT, environment.as_str()),
        (ORG, org.as_str()),
        (PROJECT, project.as_str()),
        (SCHEMA, schema.as_str()),
        (HOST_GROUP, host_group.as_str()),
        (HOST_NAME, host_name.as_str()),
        (RUNNER, runner.as_str()),
    ] {
        validate_nonempty_string(value, key)?;
    }
    let activation_identity = DevActivationIdentity {
        tenant,
        catalog,
        environment,
        org,
        project,
        schema,
        host_group,
        host_name,
        runner,
    };
    let host_binary = nonempty_path(host_binary, HOST_BINARY)?;
    let wasmtime_cache_dir = nonempty_path(wasmtime_cache_dir, WASMTIME_CACHE_DIR)?;

    validate_route_host(&route_host)?;

    let (target_probe, target_identity) =
        database_probe(TARGET_DATABASE_URL, &target_database_url)?;
    let (system_probe, system_identity) =
        database_probe(SYSTEM_DATABASE_URL, &system_database_url)?;
    let (identity_probe, identity_identity) =
        database_probe(IDENTITY_DATABASE_URL, &identity_database_url)?;
    let (guest_probe, guest_identity) = database_probe(GUEST_DATABASE_URL, &guest_database_url)?;
    let (executor_platform_probe, executor_platform_identity) = database_probe(
        EXECUTOR_PLATFORM_DATABASE_URL,
        &executor_platform_database_url,
    )?;
    let (http_admitter_probe, http_admitter_identity) =
        database_probe(HTTP_ADMITTER_DATABASE_URL, &http_admitter_database_url)?;
    let (event_materializer_probe, event_materializer_identity) = database_probe(
        EVENT_MATERIALIZER_DATABASE_URL,
        &event_materializer_database_url,
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
        ],
    )?;
    let scheduler_probe = url_probe(
        SCHEDULER_NATS_URL,
        &scheduler_nats_url,
        &["nats"],
        4222,
        false,
    )?;
    let event_probe = url_probe(EVENT_NATS_URL, &event_nats_url, &["nats"], 4222, false)?;
    let tempo_probe = url_probe(
        TEMPO_QUERY_URL,
        &tempo_query_url,
        &["http", "https"],
        3200,
        true,
    )?;
    let otel_exporter_probe = url_probe(
        OTEL_EXPORTER_OTLP_ENDPOINT,
        &otel_exporter_otlp_endpoint,
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
        scheduler_probe,
        event_probe,
        tempo_probe,
        otel_exporter_probe,
    ];

    if let Some(identity) = &session_identity {
        let valid =
            wamn_control_provision::identity_issuer::validate_identity_issuer(&identity.issuer)
                .is_ok()
                && wamn_control_provision::session_target::session_audience(
                    &wamn_control_registry::Triple::new(
                        &activation_identity.org,
                        &activation_identity.project,
                        activation_identity.environment.as_str(),
                    ),
                    &identity.instance_suffix,
                )
                .is_ok()
                && identity.ca.is_absolute();
        if !valid {
            return Err(DevConfigError::new(
                DevConfigErrorKind::MalformedDocument,
                "session_identity",
                "issuer, instance suffix, or absolute CA path refused",
            ));
        }
    }
    Ok(DevConfig {
        target_database_url,
        target_privileges_file,
        target_template_database,
        target_database_acl_file,
        system_database_url,
        identity_database_url,
        session_identity,
        guest_database_url,
        executor_platform_database_url,
        http_admitter_database_url,
        event_materializer_database_url,
        scheduler_nats_url,
        event_nats_url,
        event_nats_username,
        event_nats_password_file,
        stream_replicas,
        dup_window_secs,
        tempo_query_url,
        otel_exporter_otlp_endpoint,
        local_artifacts,
        gate_bearer_token,
        operator_bearer_token,
        route_host,
        platform_domain: platform_domain.into_boxed_str(),
        package_sources,
        effective_release_id,
        activation_identity,
        host_binary,
        wasmtime_cache_dir,
        probes: probes.into_boxed_slice(),
    })
}

/// Reach every configured service once within one finite startup budget.
pub async fn preflight_config(config: &DevConfig) -> Result<(), DevConfigError> {
    let deadline = Instant::now() + STARTUP_REACHABILITY_BUDGET;
    for probe in &config.probes {
        match timeout_at(deadline, TcpStream::connect((&*probe.host, probe.port))).await {
            Ok(Ok(stream)) => drop(stream),
            Ok(Err(source)) => {
                return Err(DevConfigError::endpoint(
                    DevConfigErrorKind::EndpointUnreachable,
                    probe.key,
                    probe.sanitized_endpoint.clone(),
                    "endpoint did not accept a connection",
                )
                .with_source(source));
            }
            Err(_) => {
                return Err(DevConfigError::endpoint(
                    DevConfigErrorKind::EndpointUnreachable,
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
    validate_package_manifest(overlay_root, &overlay_manifest)?;

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
                    DevPackageErrorKind::BaseDependencyMissing,
                    coordinate,
                    dependency.digest.as_str(),
                    &config.package_sources,
                ));
            }
            [source] => *source,
            _ => {
                return Err(DevPackageError::dependency(
                    DevPackageErrorKind::BaseDependencyAmbiguous,
                    coordinate,
                    dependency.digest.as_str(),
                    &config.package_sources,
                ));
            }
        };
        validate_package_manifest(&source.root, &source.manifest)?;
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

fn read_package_manifest(root: &Path) -> Result<PackageManifest, DevPackageError> {
    let path = root.join(PACKAGE_MANIFEST_FILE);
    let bytes = fs::read(&path).map_err(|source| {
        DevPackageError::manifest(DevPackageErrorKind::ManifestRead, path.clone(), source)
    })?;
    PackageManifest::from_slice(&bytes).map_err(|source| {
        DevPackageError::manifest(DevPackageErrorKind::ManifestInvalid, path, source)
    })
}

fn validate_package_manifest(
    root: &Path,
    manifest: &PackageManifest,
) -> Result<(), DevPackageError> {
    validate_operation_vocabulary(manifest)
        .map(|_| ())
        .map_err(|source| {
            DevPackageError::manifest(
                DevPackageErrorKind::ManifestInvalid,
                root.join(PACKAGE_MANIFEST_FILE),
                source,
            )
        })
}

fn package_source_matches(
    source: &PackageSource,
    dependency: &wamn_schema_generator::BaseDependencyRequirement,
) -> bool {
    source.manifest.package.id == dependency.package
        && source.manifest.package.version == dependency.version
}

fn validate_config_document_shape(
    object: &serde_json::Map<String, Value>,
) -> Result<(), DevConfigError> {
    let schema = dev_config_schema();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("derived dev config schema has object properties");
    for key in object.keys() {
        if !properties.contains_key(key) {
            return Err(DevConfigError::new(
                DevConfigErrorKind::UnknownKey,
                key.as_str(),
                "remove the unknown key",
            ));
        }
    }
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .expect("derived dev config schema names required properties");
    for key in required {
        let key = key
            .as_str()
            .expect("derived dev config required properties are strings");
        if !object.contains_key(key) {
            return Err(DevConfigError::new(
                DevConfigErrorKind::MissingKey,
                key,
                "supply the required deployment value",
            ));
        }
    }
    for (key, value) in object {
        if key == "session_identity" {
            serde_json::from_value::<Option<SessionIdentity>>(value.clone()).map_err(|_| {
                DevConfigError::new(
                    DevConfigErrorKind::InvalidValue,
                    key.as_str(),
                    "session identity requires issuer, ca, and instance_suffix",
                )
            })?;
            continue;
        }
        if key == LOCAL_ARTIFACTS {
            serde_json::from_value::<LocalArtifacts>(value.clone()).map_err(|_| {
                DevConfigError::new(
                    DevConfigErrorKind::InvalidValue,
                    key.as_str(),
                    "local artifacts require only directory and flow_http_component paths",
                )
            })?;
            continue;
        }
        if !json_value_matches_schema(
            properties
                .get(key)
                .expect("unknown properties were refused above"),
            value,
        ) {
            return Err(DevConfigError::new(
                DevConfigErrorKind::InvalidValue,
                key.as_str(),
                "value does not match the generated dev config schema",
            ));
        }
    }
    Ok(())
}

fn json_value_matches_schema(schema: &Value, value: &Value) -> bool {
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => value.is_string(),
        Some("boolean") => value.is_boolean(),
        Some("integer") => value.as_u64().is_some_and(|number| {
            if schema.get("format").and_then(Value::as_str) == Some("uint32")
                && u32::try_from(number).is_err()
            {
                return false;
            }
            let number = value
                .as_f64()
                .expect("unsigned JSON integer has a numeric value");
            schema
                .get("minimum")
                .and_then(Value::as_f64)
                .is_none_or(|minimum| number >= minimum)
                && schema
                    .get("maximum")
                    .and_then(Value::as_f64)
                    .is_none_or(|maximum| number <= maximum)
        }),
        Some("array") => value.as_array().is_some_and(|values| {
            schema.get("items").is_some_and(|item| {
                values
                    .iter()
                    .all(|value| json_value_matches_schema(item, value))
            })
        }),
        _ => false,
    }
}

fn nonempty_string(value: String, key: &'static str) -> Result<Box<str>, DevConfigError> {
    validate_nonempty_string(&value, key)?;
    Ok(value.into_boxed_str())
}

fn validate_nonempty_string(value: &str, key: &'static str) -> Result<(), DevConfigError> {
    if value.is_empty() {
        return Err(DevConfigError::new(
            DevConfigErrorKind::InvalidValue,
            key,
            "expected a non-empty string",
        ));
    }
    Ok(())
}

fn nonempty_path(value: PathBuf, key: &'static str) -> Result<PathBuf, DevConfigError> {
    if value.as_os_str().is_empty() {
        return Err(DevConfigError::new(
            DevConfigErrorKind::InvalidValue,
            key,
            "expected a non-empty path",
        ));
    }
    Ok(value)
}

fn validate_route_host(route_host: &str) -> Result<(), DevConfigError> {
    if route_host != "*"
        && (route_host.contains('/') || route_host.chars().any(char::is_whitespace))
    {
        return Err(DevConfigError::new(
            DevConfigErrorKind::InvalidValue,
            ROUTE_HOST,
            "expected a hostname without a path or whitespace",
        ));
    }
    Ok(())
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
            DevConfigErrorKind::InvalidValue,
            key,
            probe.sanitized_endpoint.clone(),
            "remove host, hostaddr, port, dbname, and user query overrides; use the URL authority and path",
        ));
    }
    let database_path = parsed.path().strip_prefix('/').unwrap_or_default();
    if database_path.is_empty() || database_path.contains('/') {
        return Err(DevConfigError::endpoint(
            DevConfigErrorKind::InvalidValue,
            key,
            probe.sanitized_endpoint.clone(),
            "expected one explicit database name",
        ));
    }
    let postgres = PostgresConfig::from_str(raw).map_err(|_| {
        DevConfigError::endpoint(
            DevConfigErrorKind::InvalidValue,
            key,
            probe.sanitized_endpoint.clone(),
            "expected a PostgreSQL connection URL",
        )
    })?;
    let database = postgres.get_dbname().unwrap_or_default();
    if database.is_empty() || database.contains('/') {
        return Err(DevConfigError::endpoint(
            DevConfigErrorKind::InvalidValue,
            key,
            probe.sanitized_endpoint.clone(),
            "expected one explicit database name",
        ));
    }
    Identifier::new(database).map_err(|_| {
        DevConfigError::endpoint(
            DevConfigErrorKind::InvalidValue,
            key,
            probe.sanitized_endpoint.clone(),
            "set an explicit database name of at most 63 bytes without NUL",
        )
    })?;
    let user = postgres.get_user().unwrap_or_default();
    if user.is_empty() {
        return Err(DevConfigError::endpoint(
            DevConfigErrorKind::InvalidValue,
            key,
            probe.sanitized_endpoint.clone(),
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

/// Refuse a target database the development loop must not drop.
///
/// The loop recreates the target when its schema inputs change, so a system
/// database named as the target would be destroyed by the first run.
fn validate_disposable_target_database(
    probe: &ReachabilityProbe,
    target: &DatabaseIdentity,
) -> Result<(), DevConfigError> {
    if POSTGRES_SYSTEM_DATABASES.contains(&target.database.as_ref()) {
        return Err(DevConfigError::endpoint(
            DevConfigErrorKind::DatabaseCollision,
            TARGET_DATABASE_URL,
            probe.sanitized_endpoint.clone(),
            "set target_database_url to a disposable database other than postgres, template0, or template1",
        ));
    }
    Ok(())
}

fn validate_runtime_database_credentials(
    privileged: &[(&ReachabilityProbe, &DatabaseIdentity)],
    runtime: &[(&ReachabilityProbe, &DatabaseIdentity)],
) -> Result<(), DevConfigError> {
    for (index, (probe, identity)) in runtime.iter().enumerate() {
        let collides_with_privileged = privileged
            .iter()
            .any(|(_, privileged)| identity.same_credential(privileged));
        let collides_with_runtime = runtime[..index]
            .iter()
            .any(|(_, prior)| identity.same_credential(prior));
        if collides_with_privileged || collides_with_runtime {
            return Err(DevConfigError::endpoint(
                DevConfigErrorKind::DatabaseCollision,
                probe.key,
                probe.sanitized_endpoint.clone(),
                "runtime role must use its own database credential",
            ));
        }
    }
    Ok(())
}

fn url_probe(
    key: &'static str,
    raw: &str,
    schemes: &[&str],
    default_port: u16,
    include_path: bool,
) -> Result<ReachabilityProbe, DevConfigError> {
    let parsed = parse_url(key, raw, schemes)?;
    probe_from_url(key, &parsed, default_port, include_path)
}

fn parse_url(key: &'static str, raw: &str, schemes: &[&str]) -> Result<Url, DevConfigError> {
    let parsed = Url::parse(raw).map_err(|source| {
        DevConfigError::endpoint(
            DevConfigErrorKind::InvalidValue,
            key,
            "<malformed>",
            "endpoint URL is malformed",
        )
        .with_source(source)
    })?;
    if !schemes.contains(&parsed.scheme()) {
        return Err(DevConfigError::endpoint(
            DevConfigErrorKind::InvalidValue,
            key,
            sanitized_url(&parsed, true, 0),
            "endpoint URL uses an unsupported scheme",
        ));
    }
    Ok(parsed)
}

fn probe_from_url(
    key: &'static str,
    parsed: &Url,
    default_port: u16,
    include_path: bool,
) -> Result<ReachabilityProbe, DevConfigError> {
    let host = parsed.host_str().ok_or_else(|| {
        DevConfigError::endpoint(
            DevConfigErrorKind::InvalidValue,
            key,
            "<malformed>",
            "endpoint URL has no host",
        )
    })?;
    let port = parsed.port().unwrap_or_else(|| {
        if parsed.scheme() == "http" {
            80
        } else {
            default_port
        }
    });
    Ok(ReachabilityProbe {
        key,
        host: host.into(),
        port,
        sanitized_endpoint: sanitized_url(parsed, include_path, port).into(),
    })
}

fn sanitized_url(parsed: &Url, include_path: bool, default_port: u16) -> String {
    let Some(host) = parsed.host_str() else {
        return "<malformed>".to_owned();
    };
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = parsed.port().unwrap_or(default_port);
    let path = if include_path { parsed.path() } else { "" };
    format!("{}://{host}:{port}{path}", parsed.scheme())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;
    use tokio::net::TcpListener;

    const ENDPOINT_COUNT: usize = 11;
    const DEV_CONFIG_SCHEMA_PATH: &str = "schema/wamn-dev.schema.json";
    const OVERLAY_MANIFEST: &[u8] =
        include_bytes!("../../../../../apps/client_acme_receiving/wamn.json");

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

    fn repository_package(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../apps")
            .join(name)
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
            (SCHEDULER_NATS_URL): format!("nats://{}", addresses[7]),
            (EVENT_NATS_URL): format!("nats://{}", addresses[8]),
            (EVENT_NATS_USERNAME): "dev_runtime",
            (EVENT_NATS_PASSWORD_FILE): "/run/secrets/event-nats-password",
            (STREAM_REPLICAS): 1,
            (DUP_WINDOW_SECS): 120,
            (TEMPO_QUERY_URL): format!("http://{}", addresses[9]),
            (OTEL_EXPORTER_OTLP_ENDPOINT): format!("http://{}", addresses[10]),
            (GATE_BEARER_TOKEN): "gate-super-secret",
            (ROUTE_HOST): "receiving.localhost",
            (PLATFORM_DOMAIN): "example.invalid",
            (LOCAL_ARTIFACTS): {"directory": "/tmp/wamn-local-candidate",
                "flow_http_component": "/tmp/wamn-flow-http.wasm"},
            (PACKAGE_SOURCES): [],
            (EFFECTIVE_RELEASE_ID): 1,
            (TENANT): "00000000-0000-0000-0000-000000000001",
            (CATALOG): "default",
            (ENVIRONMENT): "receiving-dev",
            (ORG): "acme",
            (PROJECT): "receiving",
            (SCHEMA): "receiving",
            (HOST_GROUP): "wamn-dev-receiving",
            (HOST_NAME): "wamn-dev-receiving-1",
            (RUNNER): "wamn-dev-receiving-1",
            (HOST_BINARY): "/opt/wamn/bin/wamn-host",
            (WASMTIME_CACHE_DIR): "/tmp/wamn-dev-cache",
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
    fn operator_token_is_optional_redacted_and_separate_from_gate_authority() {
        let address: SocketAddr = "127.0.0.1:5432".parse().unwrap();
        let mut document = complete_document(&[address; ENDPOINT_COUNT]);
        let parse = |value: &Value| parse_config(&serde_json::to_vec(value).unwrap());
        assert!(parse(&document).unwrap().operator_bearer_token().is_none());
        document[OPERATOR_BEARER_TOKEN] = Value::String("operator-private-token".into());
        let config = parse(&document).unwrap();
        assert_eq!(
            config.operator_bearer_token(),
            Some("operator-private-token")
        );
        assert_ne!(
            config.operator_bearer_token(),
            Some(config.gate_bearer_token())
        );
        assert!(!format!("{config:?}").contains("operator-private-token"));
        for invalid in [Value::Null, Value::Bool(true), Value::String(String::new())] {
            document[OPERATOR_BEARER_TOKEN] = invalid;
            assert_eq!(
                parse(&document).unwrap_err().kind(),
                DevConfigErrorKind::InvalidValue
            );
        }
    }

    #[test]
    fn event_configuration_requires_credentials_and_explicit_stream_limits() {
        let addresses = ["127.0.0.1:41000".parse().expect("test address"); ENDPOINT_COUNT];
        let document = complete_document(&addresses);
        let parsed = parse_config(&serde_json::to_vec(&document).unwrap()).unwrap();
        assert_eq!(parsed.event_nats_username(), "dev_runtime");
        assert_eq!(parsed.stream_replicas(), 1);
        assert_eq!(parsed.dup_window_secs(), 120);
        let mut long_window = document.clone();
        long_window[DUP_WINDOW_SECS] = json!(u64::from(u32::MAX) + 1);
        let parsed = parse_config(&serde_json::to_vec(&long_window).unwrap()).unwrap();
        assert_eq!(parsed.dup_window_secs(), u64::from(u32::MAX) + 1);
        for (key, value) in [
            (EVENT_NATS_USERNAME, json!("bad.user")),
            (EVENT_NATS_PASSWORD_FILE, json!("")),
            (STREAM_REPLICAS, json!(0)),
            (STREAM_REPLICAS, json!(6)),
            (DUP_WINDOW_SECS, json!(0)),
            (EFFECTIVE_RELEASE_ID, json!(u64::from(u32::MAX) + 1)),
        ] {
            let mut invalid = document.clone();
            invalid[key] = value;
            let error = parse_config(&serde_json::to_vec(&invalid).unwrap()).unwrap_err();
            assert_eq!(error.key(), key);
            assert_eq!(error.kind(), DevConfigErrorKind::InvalidValue);
        }
        for key in [
            EVENT_NATS_USERNAME,
            EVENT_NATS_PASSWORD_FILE,
            STREAM_REPLICAS,
            DUP_WINDOW_SECS,
        ] {
            let mut missing = document.clone();
            missing.as_object_mut().unwrap().remove(key);
            let error = parse_config(&serde_json::to_vec(&missing).unwrap()).unwrap_err();
            assert_eq!(error.key(), key);
            assert_eq!(error.kind(), DevConfigErrorKind::MissingKey);
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

    /// Regenerate the checked-in schema with
    /// `cargo run --locked --offline -p wamn-ctl --example print-dev-config-schema > crates/control/lib/schema/wamn-dev.schema.json`.
    #[test]
    fn checked_in_dev_config_schema_matches_generated_bytes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(DEV_CONFIG_SCHEMA_PATH);
        let checked_in = fs::read(&path).expect("read checked-in wamn dev schema");
        assert_eq!(checked_in, dev_config_schema_bytes());
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
        for expected in [
            PACKAGE_SOURCES,
            TEMPO_QUERY_URL,
            OTEL_EXPORTER_OTLP_ENDPOINT,
        ] {
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
        assert_eq!(error.kind(), DevConfigErrorKind::MissingKey);
        assert_eq!(error.key(), PACKAGE_SOURCES);

        let mut malformed = complete_document(&addresses);
        malformed[PACKAGE_SOURCES] = json!(["/valid", 7]);
        let error = parse_config(
            &serde_json::to_vec(&malformed).expect("serialize malformed package sources"),
        )
        .expect_err("package source entries must be paths");
        assert_eq!(error.kind(), DevConfigErrorKind::InvalidValue);
        assert_eq!(error.key(), PACKAGE_SOURCES);

        let config = parse_config(
            &serde_json::to_vec(&complete_document(&addresses)).expect("serialize complete config"),
        )
        .expect("complete config parses");
        assert_eq!(
            config.activation_identity().tenant,
            "00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(config.host_binary(), Path::new("/opt/wamn/bin/wamn-host"));
        assert_eq!(
            config.wasmtime_cache_dir(),
            Path::new("/tmp/wamn-dev-cache")
        );
        assert_eq!(config.effective_release_id(), 1);
        assert_eq!(config.platform_domain(), "example.invalid");
        assert_eq!(config.tempo_query_url(), format!("http://{}", addresses[9]));
        assert_eq!(
            config.otel_exporter_otlp_endpoint(),
            format!("http://{}", addresses[10])
        );

        for key in [TENANT, PLATFORM_DOMAIN, HOST_BINARY, WASMTIME_CACHE_DIR] {
            let mut missing = complete_document(&addresses);
            missing.as_object_mut().expect("fixture object").remove(key);
            let error = parse_config(
                &serde_json::to_vec(&missing).expect("serialize missing required input"),
            )
            .expect_err("every identity and local-host input is required");
            assert_eq!(error.kind(), DevConfigErrorKind::MissingKey);
            assert_eq!(error.key(), key);
        }

        let mut invalid_domain = complete_document(&addresses);
        invalid_domain[PLATFORM_DOMAIN] = json!("example.invalid/path");
        let error = parse_config(
            &serde_json::to_vec(&invalid_domain).expect("serialize invalid platform domain"),
        )
        .expect_err("the platform domain must be a domain name");
        assert_eq!(error.kind(), DevConfigErrorKind::InvalidValue);
        assert_eq!(error.key(), PLATFORM_DOMAIN);

        let mut zero_release = complete_document(&addresses);
        zero_release[EFFECTIVE_RELEASE_ID] = json!(0);
        let error = parse_config(
            &serde_json::to_vec(&zero_release).expect("serialize zero release identity"),
        )
        .expect_err("effective release identity must be positive");
        assert_eq!(error.kind(), DevConfigErrorKind::InvalidValue);
        assert_eq!(error.key(), EFFECTIVE_RELEASE_ID);
    }

    #[test]
    fn overlay_dependencies_resolve_by_coordinate_and_report_ignored_sources() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        let overlay_root = repository_package("client_acme_receiving");
        let base_root = repository_package("wamn_receiving");
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
            "client_acme_receiving"
        );
        assert_eq!(resolved.base_packages().len(), 1);
        assert_eq!(resolved.ignored_package_source_count(), 1);
        assert_eq!(resolved.ignored_package_sources(), [overlay_root]);
        let base = &resolved.base_packages()[0];
        assert_eq!(base.alias(), "base_receiving");
        assert_eq!(base.root(), repository_package("wamn_receiving"));
        assert_eq!(base.manifest().package.id, "wamn_receiving");
        assert_eq!(
            base.component_digest().expected(),
            resolved.overlay_manifest().base_dependencies["base_receiving"].digest
        );

        let verified = base
            .component_digest()
            .verify(base.component_digest().expected());
        assert_eq!(verified.coordinate(), "wamn_receiving@1.0.0");
        assert_eq!(verified.digest(), base.component_digest().expected());
        assert_eq!(verified.superseded_pin(), None);
    }

    fn moved_digest_expectation() -> (BaseComponentDigestExpectation, String) {
        let expectation = BaseComponentDigestExpectation {
            coordinate: "wamn_receiving@1.0.0".into(),
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
        let overlay_root = repository_package("client_acme_receiving");
        let base_root = repository_package("wamn_receiving");
        let expected = wamn_schema_generator::PackageManifest::from_slice(OVERLAY_MANIFEST)
            .expect("parse repository overlay")
            .base_dependencies
            .remove("base_receiving")
            .expect("overlay declares its base");

        let mut missing_document = complete_document(&addresses);
        missing_document[PACKAGE_SOURCES] = json!([overlay_root]);
        let missing_config = parse_config(
            &serde_json::to_vec(&missing_document).expect("serialize missing-source config"),
        )
        .expect("missing source is a resolution concern");
        let error = resolve_dev_packages(&missing_config, &overlay_root)
            .expect_err("zero coordinate matches must refuse");
        assert_eq!(error.kind(), DevPackageErrorKind::BaseDependencyMissing);
        assert_eq!(error.coordinate(), Some("wamn_receiving@1.0.0"));
        assert_eq!(error.dependency_digest(), Some(expected.digest.as_str()));
        assert_eq!(error.searched_roots(), std::slice::from_ref(&overlay_root));

        let mut ambiguous_document = complete_document(&addresses);
        ambiguous_document[PACKAGE_SOURCES] =
            json!([base_root.clone(), overlay_root, base_root.clone()]);
        let ambiguous_config = parse_config(
            &serde_json::to_vec(&ambiguous_document).expect("serialize ambiguous-source config"),
        )
        .expect("duplicate coordinates are a resolution concern");
        let error = resolve_dev_packages(
            &ambiguous_config,
            &repository_package("client_acme_receiving"),
        )
        .expect_err("multiple coordinate matches must refuse");
        assert_eq!(error.kind(), DevPackageErrorKind::BaseDependencyAmbiguous);
        assert_eq!(error.coordinate(), Some("wamn_receiving@1.0.0"));
        assert_eq!(error.dependency_digest(), Some(expected.digest.as_str()));
        assert_eq!(
            error.searched_roots(),
            [
                base_root.clone(),
                repository_package("client_acme_receiving"),
                base_root
            ]
        );
    }

    #[test]
    fn overlay_manifest_is_parsed_through_the_strict_package_parser() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        let mut overlay: Value =
            serde_json::from_slice(OVERLAY_MANIFEST).expect("parse overlay fixture");
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

        assert_eq!(error.kind(), DevPackageErrorKind::ManifestInvalid);
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
            "gate-super-secret",
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

        assert_eq!(error.kind(), DevConfigErrorKind::EndpointUnreachable);
        assert_eq!(error.key(), TARGET_DATABASE_URL);
        assert_eq!(
            error.sanitized_endpoint(),
            Some(format!("postgresql://{address}/target").as_str())
        );
        let message = error.to_string();
        assert!(!message.contains("target-secret"));
        assert!(!message.contains("gate-super-secret"));
    }

    #[test]
    fn malformed_unknown_and_missing_inputs_refuse_at_their_exact_key() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        let mut malformed = complete_document(&addresses);
        malformed[TARGET_DATABASE_URL] = json!("postgresql://user:secret@[");
        let error = parse_config(&serde_json::to_vec(&malformed).expect("serialize malformed"))
            .expect_err("malformed endpoint must refuse");
        assert_eq!(error.kind(), DevConfigErrorKind::InvalidValue);
        assert_eq!(error.key(), TARGET_DATABASE_URL);
        assert_eq!(error.sanitized_endpoint(), Some("<malformed>"));
        assert!(!error.to_string().contains("secret"));

        let mut unknown = complete_document(&addresses);
        unknown["project_database_url"] = json!("postgresql://ignored:secret@invalid/db");
        let error = parse_config(&serde_json::to_vec(&unknown).expect("serialize unknown"))
            .expect_err("unknown key must refuse");
        assert_eq!(error.kind(), DevConfigErrorKind::UnknownKey);
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
        assert_eq!(error.kind(), DevConfigErrorKind::MissingKey);
        assert_eq!(error.key(), SCHEDULER_NATS_URL);

        for key in [TEMPO_QUERY_URL, OTEL_EXPORTER_OTLP_ENDPOINT] {
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
            assert_eq!(error.kind(), DevConfigErrorKind::MissingKey);
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
        assert_eq!(error.kind(), DevConfigErrorKind::MissingKey);
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
        assert_eq!(error.kind(), DevConfigErrorKind::MissingKey);
        assert_eq!(error.key(), EVENT_MATERIALIZER_DATABASE_URL);

        let mut tls_scheduler = complete_document(&addresses);
        tls_scheduler[SCHEDULER_NATS_URL] = json!("tls://scheduler.invalid:4222");
        let error = parse_config(
            &serde_json::to_vec(&tls_scheduler).expect("serialize unsupported TLS endpoint"),
        )
        .expect_err("TLS without trust configuration must refuse");
        assert_eq!(error.kind(), DevConfigErrorKind::InvalidValue);
        assert_eq!(error.key(), SCHEDULER_NATS_URL);

        for key in [TEMPO_QUERY_URL, OTEL_EXPORTER_OTLP_ENDPOINT] {
            let mut unsupported_observability_endpoint = complete_document(&addresses);
            unsupported_observability_endpoint[key] = json!("nats://127.0.0.1:41000");
            let error = parse_config(
                &serde_json::to_vec(&unsupported_observability_endpoint)
                    .expect("serialize unsupported observability endpoint"),
            )
            .expect_err("observability endpoints require HTTP or HTTPS");
            assert_eq!(error.kind(), DevConfigErrorKind::InvalidValue);
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

            assert_eq!(error.kind(), DevConfigErrorKind::InvalidValue);
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
        assert_eq!(error.kind(), DevConfigErrorKind::DatabaseCollision);
        assert_eq!(error.key(), GUEST_DATABASE_URL);
        assert!(!error.to_string().contains("target-secret"));

        let mut sibling_reuse = complete_document(&addresses);
        let platform_credential = sibling_reuse[EXECUTOR_PLATFORM_DATABASE_URL].clone();
        sibling_reuse[EVENT_MATERIALIZER_DATABASE_URL] = platform_credential;
        let error =
            parse_config(&serde_json::to_vec(&sibling_reuse).expect("serialize sibling reuse"))
                .expect_err("runtime roles must not share one credential");
        assert_eq!(error.kind(), DevConfigErrorKind::DatabaseCollision);
        assert_eq!(error.key(), EVENT_MATERIALIZER_DATABASE_URL);
        assert!(!error.to_string().contains("platform-secret"));
    }

    #[test]
    fn malformed_json_refuses_without_inspecting_partial_credentials() {
        let error = parse_config(br#"{"gate_bearer_token":"secret""#)
            .expect_err("malformed JSON must refuse");
        assert_eq!(error.kind(), DevConfigErrorKind::MalformedDocument);
        assert_eq!(error.key(), DOCUMENT_KEY);
        assert!(!error.to_string().contains("secret"));
    }
}
