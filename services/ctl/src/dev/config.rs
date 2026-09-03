//! Strict deployment-owned configuration for the development loop.
//!
//! This module validates and preflights externally supplied services. It does
//! not provision them or execute any development stage.

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use oci_client::Reference;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout_at};
use tokio_postgres::Config as PostgresConfig;
use url::Url;
use wamn_pg_core::Identifier;
use wamn_runtime::component_artifact::component_artifact_reference;
use wamn_schema_generator::{PackageManifest, validate_operation_vocabulary};

use super::activation::DevActivationIdentity;

/// Whole-startup budget shared by every configured reachability probe.
pub const STARTUP_REACHABILITY_BUDGET: Duration = Duration::from_secs(5);

const DOCUMENT_KEY: &str = "$";
pub(super) const VERIFICATION_DATABASE_URL: &str = "verification_database_url";
const TARGET_DATABASE_URL: &str = "target_database_url";
const SYSTEM_DATABASE_URL: &str = "system_database_url";
const IDENTITY_DATABASE_URL: &str = "identity_database_url";
const GUEST_DATABASE_URL: &str = "guest_database_url";
const EXECUTOR_PLATFORM_DATABASE_URL: &str = "executor_platform_database_url";
const HTTP_ADMITTER_DATABASE_URL: &str = "http_admitter_database_url";
const EVENT_MATERIALIZER_DATABASE_URL: &str = "event_materializer_database_url";
const SCHEDULER_NATS_URL: &str = "scheduler_nats_url";
const EVENT_NATS_URL: &str = "event_nats_url";
const COMPONENT_ARTIFACT_BASE: &str = "component_artifact_base";
const RELEASE_ARTIFACT_BASE: &str = "release_artifact_base";
const REGISTRY_AUTH_FILE: &str = "registry_auth_file";
const INSECURE_REGISTRY: &str = "insecure_registry";
const GATE_URL: &str = "gate_url";
const GATE_BEARER_TOKEN: &str = "gate_bearer_token";
const ROUTE_HOST: &str = "route_host";
const FLOW_HTTP_WORKLOAD_IMAGE: &str = "flow_http_workload_image";
const PACKAGE_SOURCES: &str = "package_sources";
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

const REFERENCE_PROBE_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

/// Sole field authority for the strict deployment-owned `dev.json` document.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DevConfigDocument {
    verification_database_url: String,
    target_database_url: String,
    system_database_url: String,
    identity_database_url: String,
    guest_database_url: String,
    executor_platform_database_url: String,
    http_admitter_database_url: String,
    event_materializer_database_url: String,
    scheduler_nats_url: String,
    event_nats_url: String,
    component_artifact_base: String,
    release_artifact_base: String,
    registry_auth_file: PathBuf,
    insecure_registry: bool,
    gate_url: String,
    gate_bearer_token: String,
    route_host: String,
    flow_http_workload_image: String,
    package_sources: Vec<PathBuf>,
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
    ComponentDigestMismatch,
}

impl DevPackageErrorKind {
    /// Stable diagnostic code for this error category.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ManifestRead => "dev-package-manifest-read",
            Self::ManifestInvalid => "dev-package-manifest-invalid",
            Self::BaseDependencyMissing => "dev-base-dependency-missing",
            Self::BaseDependencyAmbiguous => "dev-base-dependency-ambiguous",
            Self::ComponentDigestMismatch => "dev-base-component-digest-mismatch",
        }
    }
}

/// Refusal to resolve a manifest-declared package or prove its built component.
#[derive(Debug)]
pub struct DevPackageError {
    kind: DevPackageErrorKind,
    manifest_path: Option<PathBuf>,
    coordinate: Option<Box<str>>,
    dependency_digest: Option<Box<str>>,
    observed_digest: Option<Box<str>>,
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
            observed_digest: None,
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
            observed_digest: None,
            searched_roots: searched_roots.into(),
            source: None,
        }
    }

    fn digest_mismatch(
        coordinate: impl Into<Box<str>>,
        expected: impl Into<Box<str>>,
        observed: impl Into<Box<str>>,
    ) -> Self {
        Self {
            kind: DevPackageErrorKind::ComponentDigestMismatch,
            manifest_path: None,
            coordinate: Some(coordinate.into()),
            dependency_digest: Some(expected.into()),
            observed_digest: Some(observed.into()),
            searched_roots: Box::new([]),
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

    /// Built component digest that failed exact comparison.
    pub fn observed_digest(&self) -> Option<&str> {
        self.observed_digest.as_deref()
    }

    /// Complete candidate-root inventory searched for this dependency.
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
        if let Some(observed) = &self.observed_digest {
            write!(formatter, ", observed {observed}")?;
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

/// Manifest-owned component digest that a built base component must equal.
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

    /// Admit a built component only when its digest equals the manifest declaration.
    pub fn verify(
        &self,
        observed: impl Into<Box<str>>,
    ) -> Result<VerifiedBaseComponentDigest, DevPackageError> {
        let observed = observed.into();
        if observed != self.expected {
            return Err(DevPackageError::digest_mismatch(
                self.coordinate.clone(),
                self.expected.clone(),
                observed,
            ));
        }
        Ok(VerifiedBaseComponentDigest {
            coordinate: self.coordinate.clone(),
            digest: self.expected.clone(),
        })
    }
}

/// Proof that a built base component equals its manifest-declared digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedBaseComponentDigest {
    coordinate: Box<str>,
    digest: Box<str>,
}

impl VerifiedBaseComponentDigest {
    /// Exact base-package coordinate whose component was verified.
    pub fn coordinate(&self) -> &str {
        &self.coordinate
    }

    /// Expected and observed component digest after exact equality succeeds.
    pub fn digest(&self) -> &str {
        &self.digest
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

    /// Component digest that the base build must prove before Gate or Publish.
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
    fn same_database_name(&self, other: &Self) -> bool {
        self.database == other.database
    }

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
    verification_database_url: Box<str>,
    target_database_url: Box<str>,
    system_database_url: Box<str>,
    identity_database_url: Box<str>,
    guest_database_url: Box<str>,
    executor_platform_database_url: Box<str>,
    http_admitter_database_url: Box<str>,
    event_materializer_database_url: Box<str>,
    scheduler_nats_url: Box<str>,
    event_nats_url: Box<str>,
    component_artifact_base: Box<str>,
    release_artifact_base: Box<str>,
    registry_auth_file: PathBuf,
    insecure_registry: bool,
    gate_url: Box<str>,
    gate_bearer_token: Box<str>,
    route_host: Box<str>,
    flow_http_workload_image: Box<str>,
    package_sources: Box<[PathBuf]>,
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
                VERIFICATION_DATABASE_URL,
                &self.sanitized_endpoint(VERIFICATION_DATABASE_URL),
            )
            .field(
                TARGET_DATABASE_URL,
                &self.sanitized_endpoint(TARGET_DATABASE_URL),
            )
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
            .field(
                COMPONENT_ARTIFACT_BASE,
                &self.sanitized_endpoint(COMPONENT_ARTIFACT_BASE),
            )
            .field(
                RELEASE_ARTIFACT_BASE,
                &self.sanitized_endpoint(RELEASE_ARTIFACT_BASE),
            )
            .field(REGISTRY_AUTH_FILE, &self.registry_auth_file)
            .field(INSECURE_REGISTRY, &self.insecure_registry)
            .field(GATE_URL, &self.sanitized_endpoint(GATE_URL))
            .field(GATE_BEARER_TOKEN, &"[REDACTED]")
            .field(ROUTE_HOST, &self.route_host)
            .field(
                FLOW_HTTP_WORKLOAD_IMAGE,
                &self.sanitized_endpoint(FLOW_HTTP_WORKLOAD_IMAGE),
            )
            .field(PACKAGE_SOURCES, &self.package_sources)
            .field("activation_identity", &self.activation_identity)
            .field(HOST_BINARY, &self.host_binary)
            .field(WASMTIME_CACHE_DIR, &self.wasmtime_cache_dir)
            .finish()
    }
}

impl DevConfig {
    fn sanitized_endpoint(&self, key: &str) -> &str {
        self.probes
            .iter()
            .find(|probe| probe.key == key)
            .map_or("<not-an-endpoint>", |probe| &probe.sanitized_endpoint)
    }

    /// Verification-only PostgreSQL URL used by generation and native checks.
    pub fn verification_database_url(&self) -> &str {
        &self.verification_database_url
    }

    /// Durable target PostgreSQL URL used only after provenance gates pass.
    pub fn target_database_url(&self) -> &str {
        &self.target_database_url
    }

    /// System PostgreSQL URL holding control and identity facts.
    pub fn system_database_url(&self) -> &str {
        &self.system_database_url
    }

    /// Identity-reader PostgreSQL URL passed to the local serving host.
    pub fn identity_database_url(&self) -> &str {
        &self.identity_database_url
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

    /// Explicit component registry and repository base.
    pub fn component_artifact_base(&self) -> &str {
        &self.component_artifact_base
    }

    /// Explicit release registry and repository base.
    pub fn release_artifact_base(&self) -> &str {
        &self.release_artifact_base
    }

    /// Deployment-owned Docker authentication document for both registries.
    pub fn registry_auth_file(&self) -> &Path {
        &self.registry_auth_file
    }

    /// Whether the development registries use plain HTTP.
    pub const fn insecure_registry(&self) -> bool {
        self.insecure_registry
    }

    /// Authenticated authoring Gate endpoint.
    pub fn gate_url(&self) -> &str {
        &self.gate_url
    }

    /// Bearer credential presented only to the Gate.
    pub fn gate_bearer_token(&self) -> &str {
        &self.gate_bearer_token
    }

    /// Deployment-owned route hostname supplied at publication.
    pub fn route_host(&self) -> &str {
        &self.route_host
    }

    /// OCI image reference started through the native workload API.
    pub fn flow_http_workload_image(&self) -> &str {
        &self.flow_http_workload_image
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
        verification_database_url,
        target_database_url,
        system_database_url,
        identity_database_url,
        guest_database_url,
        executor_platform_database_url,
        http_admitter_database_url,
        event_materializer_database_url,
        scheduler_nats_url,
        event_nats_url,
        component_artifact_base,
        release_artifact_base,
        registry_auth_file,
        insecure_registry,
        gate_url,
        gate_bearer_token,
        route_host,
        flow_http_workload_image,
        package_sources,
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

    let verification_database_url =
        nonempty_string(verification_database_url, VERIFICATION_DATABASE_URL)?;
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
    let component_artifact_base =
        nonempty_string(component_artifact_base, COMPONENT_ARTIFACT_BASE)?;
    let release_artifact_base = nonempty_string(release_artifact_base, RELEASE_ARTIFACT_BASE)?;
    let registry_auth_file = nonempty_path(registry_auth_file, REGISTRY_AUTH_FILE)?;
    let gate_url = nonempty_string(gate_url, GATE_URL)?;
    let gate_bearer_token = nonempty_string(gate_bearer_token, GATE_BEARER_TOKEN)?;
    let route_host = nonempty_string(route_host, ROUTE_HOST)?;
    let flow_http_workload_image =
        nonempty_string(flow_http_workload_image, FLOW_HTTP_WORKLOAD_IMAGE)?;
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

    let (verification_probe, verification_identity) =
        database_probe(VERIFICATION_DATABASE_URL, &verification_database_url)?;
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
    validate_verification_database(
        &verification_probe,
        &verification_identity,
        &[
            &target_identity,
            &system_identity,
            &identity_identity,
            &guest_identity,
            &executor_platform_identity,
            &http_admitter_identity,
            &event_materializer_identity,
        ],
    )?;
    validate_runtime_database_credentials(
        &[
            (&verification_probe, &verification_identity),
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
    let registry_port = if insecure_registry { 80 } else { 443 };
    let component_probe = artifact_base_probe(
        COMPONENT_ARTIFACT_BASE,
        &component_artifact_base,
        registry_port,
    )?;
    let release_probe =
        artifact_base_probe(RELEASE_ARTIFACT_BASE, &release_artifact_base, registry_port)?;
    let gate_probe = url_probe(GATE_URL, &gate_url, &["http", "https"], 443, true)?;
    let flow_http_probe = workload_image_probe(
        FLOW_HTTP_WORKLOAD_IMAGE,
        &flow_http_workload_image,
        registry_port,
    )?;

    Ok(DevConfig {
        verification_database_url,
        target_database_url,
        system_database_url,
        identity_database_url,
        guest_database_url,
        executor_platform_database_url,
        http_admitter_database_url,
        event_materializer_database_url,
        scheduler_nats_url,
        event_nats_url,
        component_artifact_base,
        release_artifact_base,
        registry_auth_file,
        insecure_registry,
        gate_url,
        gate_bearer_token,
        route_host,
        flow_http_workload_image,
        package_sources,
        activation_identity,
        host_binary,
        wasmtime_cache_dir,
        probes: vec![
            verification_probe,
            target_probe,
            system_probe,
            identity_probe,
            guest_probe,
            executor_platform_probe,
            http_admitter_probe,
            event_materializer_probe,
            scheduler_probe,
            event_probe,
            component_probe,
            release_probe,
            gate_probe,
            flow_http_probe,
        ]
        .into_boxed_slice(),
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

fn validate_verification_database(
    probe: &ReachabilityProbe,
    verification: &DatabaseIdentity,
    protected: &[&DatabaseIdentity],
) -> Result<(), DevConfigError> {
    if POSTGRES_SYSTEM_DATABASES.contains(&verification.database.as_ref()) {
        return Err(DevConfigError::endpoint(
            DevConfigErrorKind::DatabaseCollision,
            VERIFICATION_DATABASE_URL,
            probe.sanitized_endpoint.clone(),
            "set verification_database_url to a disposable database other than postgres, template0, or template1",
        ));
    }
    // DNS, IP, and service aliases cannot be proven disjoint here. Refusing a
    // reused name costs naming flexibility; trusting aliases could DROP a
    // protected database when the disposable verification database is reset.
    if protected
        .iter()
        .any(|identity| verification.same_database_name(identity))
    {
        return Err(DevConfigError::endpoint(
            DevConfigErrorKind::DatabaseCollision,
            VERIFICATION_DATABASE_URL,
            probe.sanitized_endpoint.clone(),
            "set verification_database_url to a disposable database distinct from every target, system, and runtime database",
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

fn artifact_base_probe(
    key: &'static str,
    raw: &str,
    default_port: u16,
) -> Result<ReachabilityProbe, DevConfigError> {
    let reference =
        component_artifact_reference(raw, REFERENCE_PROBE_DIGEST).map_err(|source| {
            DevConfigError::endpoint(
                DevConfigErrorKind::InvalidValue,
                key,
                sanitized_registry_hint(raw, default_port),
                "expected an explicit <registry>/<repository> base",
            )
            .with_source(source)
        })?;
    authority_probe(key, reference.registry(), default_port)
}

fn workload_image_probe(
    key: &'static str,
    raw: &str,
    default_port: u16,
) -> Result<ReachabilityProbe, DevConfigError> {
    let first = raw.split('/').next().unwrap_or_default();
    if !(first.contains('.') || first.contains(':') || first == "localhost") {
        return Err(DevConfigError::endpoint(
            DevConfigErrorKind::InvalidValue,
            key,
            "<malformed>",
            "workload image requires an explicit registry",
        ));
    }
    let reference = Reference::try_from(raw).map_err(|_| {
        DevConfigError::endpoint(
            DevConfigErrorKind::InvalidValue,
            key,
            sanitized_registry_hint(raw, default_port),
            "workload image reference is malformed",
        )
    })?;
    authority_probe(key, reference.registry(), default_port)
}

fn authority_probe(
    key: &'static str,
    authority: &str,
    default_port: u16,
) -> Result<ReachabilityProbe, DevConfigError> {
    let parsed = Url::parse(&format!("tcp://{authority}")).map_err(|_| {
        DevConfigError::endpoint(
            DevConfigErrorKind::InvalidValue,
            key,
            "<malformed>",
            "registry authority is malformed",
        )
    })?;
    let host = parsed.host_str().ok_or_else(|| {
        DevConfigError::endpoint(
            DevConfigErrorKind::InvalidValue,
            key,
            "<malformed>",
            "registry authority has no host",
        )
    })?;
    let port = parsed.port().unwrap_or(default_port);
    let host_label = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    Ok(ReachabilityProbe {
        key,
        host: host.into(),
        port,
        sanitized_endpoint: format!("oci://{host_label}:{port}").into(),
    })
}

fn sanitized_registry_hint(raw: &str, default_port: u16) -> Box<str> {
    let authority = raw.split('/').next().unwrap_or_default();
    let parsed = Url::parse(&format!("tcp://{authority}"));
    let Ok(parsed) = parsed else {
        return "<malformed>".into();
    };
    let Some(host) = parsed.host_str() else {
        return "<malformed>".into();
    };
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    format!("oci://{host}:{}", parsed.port().unwrap_or(default_port)).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;
    use tokio::net::TcpListener;

    const ENDPOINT_COUNT: usize = 14;
    const DEV_CONFIG_SCHEMA_PATH: &str = "schema/wamn-dev.schema.json";
    const OVERLAY_MANIFEST: &[u8] =
        include_bytes!("../../../../packages/client_acme_receiving/wamn.json");

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
            .join("../../packages")
            .join(name)
    }

    fn complete_document(addresses: &[SocketAddr; ENDPOINT_COUNT]) -> Value {
        json!({
            (VERIFICATION_DATABASE_URL): format!("postgresql://verify:verify-secret@{}/verification", addresses[0]),
            (TARGET_DATABASE_URL): format!("postgresql://target:target-secret@{}/target", addresses[1]),
            (SYSTEM_DATABASE_URL): format!("postgresql://system:system-secret@{}/system", addresses[2]),
            (IDENTITY_DATABASE_URL): format!("postgresql://identity:identity-secret@{}/system", addresses[3]),
            (GUEST_DATABASE_URL): format!("postgresql://guest:guest-secret@{}/target", addresses[4]),
            (EXECUTOR_PLATFORM_DATABASE_URL): format!("postgresql://platform:platform-secret@{}/target", addresses[5]),
            (HTTP_ADMITTER_DATABASE_URL): format!("postgresql://admitter:admitter-secret@{}/target", addresses[6]),
            (EVENT_MATERIALIZER_DATABASE_URL): format!("postgresql://materializer:materializer-secret@{}/target", addresses[7]),
            (SCHEDULER_NATS_URL): format!("nats://{}", addresses[8]),
            (EVENT_NATS_URL): format!("nats://{}", addresses[9]),
            (COMPONENT_ARTIFACT_BASE): format!("{}/wamn/components", addresses[10]),
            (RELEASE_ARTIFACT_BASE): format!("{}/wamn/releases", addresses[11]),
            (REGISTRY_AUTH_FILE): "/run/secrets/registry.json",
            (INSECURE_REGISTRY): true,
            (GATE_URL): format!("http://{}/authoring", addresses[12]),
            (GATE_BEARER_TOKEN): "gate-super-secret",
            (ROUTE_HOST): "receiving.localhost",
            (FLOW_HTTP_WORKLOAD_IMAGE): format!("{}/wamn/flow-http:dev", addresses[13]),
            (PACKAGE_SOURCES): [],
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
    fn checked_in_dev_config_schema_matches_generated_bytes() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(DEV_CONFIG_SCHEMA_PATH);
        let checked_in = fs::read(&path).expect("read checked-in wamn dev schema");
        assert_eq!(checked_in, dev_config_schema_bytes());
    }

    #[test]
    #[ignore = "schema regeneration command only"]
    fn regenerate_checked_in_dev_config_schema() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(DEV_CONFIG_SCHEMA_PATH);
        fs::write(path, dev_config_schema_bytes()).expect("write generated wamn dev schema");
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
        assert!(
            schema["required"]
                .as_array()
                .expect("schema required set")
                .iter()
                .any(|key| key == PACKAGE_SOURCES)
        );

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

        for key in [TENANT, HOST_BINARY, WASMTIME_CACHE_DIR] {
            let mut missing = complete_document(&addresses);
            missing.as_object_mut().expect("fixture object").remove(key);
            let error = parse_config(
                &serde_json::to_vec(&missing).expect("serialize missing required input"),
            )
            .expect_err("every identity and local-host input is required");
            assert_eq!(error.kind(), DevConfigErrorKind::MissingKey);
            assert_eq!(error.key(), key);
        }
    }

    #[test]
    fn overlay_dependencies_resolve_by_coordinate_and_report_ignored_sources() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        let overlay_root = repository_package("client_acme_receiving");
        let base_root = repository_package("receiving");
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
        assert_eq!(base.root(), repository_package("receiving"));
        assert_eq!(base.manifest().package.id, "wamn_receiving");
        assert_eq!(
            base.component_digest().expected(),
            resolved.overlay_manifest().base_dependencies["base_receiving"].digest
        );

        let verified = base
            .component_digest()
            .verify(base.component_digest().expected())
            .expect("exact built digest verifies");
        assert_eq!(verified.coordinate(), "wamn_receiving@1.0.0");
        assert_eq!(verified.digest(), base.component_digest().expected());

        let observed = format!("sha256:{}", "0".repeat(64));
        let error = base
            .component_digest()
            .verify(observed.as_str())
            .expect_err("a different built component must refuse before Gate or Publish");
        assert_eq!(error.kind(), DevPackageErrorKind::ComponentDigestMismatch);
        assert_eq!(error.coordinate(), Some("wamn_receiving@1.0.0"));
        assert_eq!(
            error.dependency_digest(),
            Some(base.component_digest().expected())
        );
        assert_eq!(error.observed_digest(), Some(observed.as_str()));
    }

    #[test]
    fn missing_and_ambiguous_dependencies_name_the_complete_search() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        let overlay_root = repository_package("client_acme_receiving");
        let base_root = repository_package("receiving");
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
        assert_eq!(error.searched_roots(), [overlay_root.clone()]);

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
            "verify-secret",
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
        assert_eq!(error.key(), VERIFICATION_DATABASE_URL);
        assert_eq!(
            error.sanitized_endpoint(),
            Some(format!("postgresql://{address}/verification").as_str())
        );
        let message = error.to_string();
        assert!(!message.contains("verify-secret"));
        assert!(!message.contains("gate-super-secret"));
    }

    #[test]
    fn malformed_unknown_and_missing_inputs_refuse_at_their_exact_key() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        let mut malformed = complete_document(&addresses);
        malformed[VERIFICATION_DATABASE_URL] = json!("postgresql://user:secret@[");
        let error = parse_config(&serde_json::to_vec(&malformed).expect("serialize malformed"))
            .expect_err("malformed endpoint must refuse");
        assert_eq!(error.kind(), DevConfigErrorKind::InvalidValue);
        assert_eq!(error.key(), VERIFICATION_DATABASE_URL);
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

        let mut missing_workload = complete_document(&addresses);
        missing_workload
            .as_object_mut()
            .expect("fixture object")
            .remove(FLOW_HTTP_WORKLOAD_IMAGE);
        let error = parse_config(
            &serde_json::to_vec(&missing_workload).expect("serialize missing workload"),
        )
        .expect_err("missing workload image must refuse");
        assert_eq!(error.kind(), DevConfigErrorKind::MissingKey);
        assert_eq!(error.key(), FLOW_HTTP_WORKLOAD_IMAGE);

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
    }

    #[test]
    fn verification_database_cannot_alias_a_protected_database() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        for protected_key in [
            TARGET_DATABASE_URL,
            SYSTEM_DATABASE_URL,
            IDENTITY_DATABASE_URL,
            GUEST_DATABASE_URL,
            EXECUTOR_PLATFORM_DATABASE_URL,
            HTTP_ADMITTER_DATABASE_URL,
            EVENT_MATERIALIZER_DATABASE_URL,
        ] {
            let mut document = complete_document(&addresses);
            let mut alias = Url::parse(
                document[protected_key]
                    .as_str()
                    .expect("protected URL fixture"),
            )
            .expect("parse protected URL fixture");
            alias
                .set_username("verifier")
                .expect("replace fixture username");
            alias
                .set_password(Some("verify-secret"))
                .expect("replace fixture password");
            document[VERIFICATION_DATABASE_URL] = json!(alias.as_str());

            let error = parse_config(&serde_json::to_vec(&document).expect("serialize collision"))
                .expect_err("verification database alias must refuse");

            assert_eq!(error.kind(), DevConfigErrorKind::DatabaseCollision);
            assert_eq!(error.key(), VERIFICATION_DATABASE_URL);
            let message = error.to_string();
            assert!(message.contains("set verification_database_url"));
            assert!(!message.contains("secret"));
        }
    }

    #[test]
    fn verification_database_refuses_postgres_system_names_and_encoded_aliases() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        for database in POSTGRES_SYSTEM_DATABASES {
            let mut document = complete_document(&addresses);
            document[VERIFICATION_DATABASE_URL] = json!(format!(
                "postgresql://verifier:verify-secret@127.0.0.1:41000/{database}"
            ));

            let error =
                parse_config(&serde_json::to_vec(&document).expect("serialize system name"))
                    .expect_err("a PostgreSQL system database must refuse");

            assert_eq!(error.kind(), DevConfigErrorKind::DatabaseCollision);
            assert_eq!(error.key(), VERIFICATION_DATABASE_URL);
            assert!(error.to_string().contains("set verification_database_url"));
            assert!(!error.to_string().contains("verify-secret"));
        }

        let mut encoded_alias = complete_document(&addresses);
        encoded_alias[VERIFICATION_DATABASE_URL] =
            json!("postgresql://verifier:verify-secret@127.0.0.1:41000/shared%2Ddatabase");
        encoded_alias[TARGET_DATABASE_URL] =
            json!("postgresql://target:target-secret@127.0.0.1:41000/shared-database");
        let error =
            parse_config(&serde_json::to_vec(&encoded_alias).expect("serialize encoded alias"))
                .expect_err("encoded database alias must refuse");
        assert_eq!(error.kind(), DevConfigErrorKind::DatabaseCollision);
        assert_eq!(error.key(), VERIFICATION_DATABASE_URL);
    }

    #[test]
    fn verification_database_name_collision_refuses_across_distinct_host_labels() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        let mut document = complete_document(&addresses);
        document[VERIFICATION_DATABASE_URL] =
            json!("postgresql://verifier:verify-secret@verification.invalid:41000/shared-database");
        document[TARGET_DATABASE_URL] =
            json!("postgresql://target:target-secret@target.invalid:41000/shared-database");

        let error = parse_config(&serde_json::to_vec(&document).expect("serialize collision"))
            .expect_err("host aliases cannot make a destructive database name safe");

        assert_eq!(error.kind(), DevConfigErrorKind::DatabaseCollision);
        assert_eq!(error.key(), VERIFICATION_DATABASE_URL);
        assert!(!error.to_string().contains("secret"));
    }

    #[test]
    fn postgres_identity_routing_query_overrides_refuse_without_leaking_values() {
        let addresses = ["127.0.0.1:41000".parse().expect("fixture address"); ENDPOINT_COUNT];
        for query_key in POSTGRES_ROUTING_QUERY_KEYS {
            let mut document = complete_document(&addresses);
            document[VERIFICATION_DATABASE_URL] = json!(format!(
                "postgresql://verifier:verify-secret@127.0.0.1:41000/verification?{query_key}=override-secret"
            ));

            let error =
                parse_config(&serde_json::to_vec(&document).expect("serialize routing override"))
                    .expect_err("routing query override must refuse");

            assert_eq!(error.kind(), DevConfigErrorKind::InvalidValue);
            assert_eq!(error.key(), VERIFICATION_DATABASE_URL);
            assert!(error.to_string().contains("remove host, hostaddr, port"));
            assert!(!error.to_string().contains("override-secret"));
            assert!(!error.to_string().contains("verify-secret"));
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
