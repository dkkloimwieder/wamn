//! Strict deployment-owned configuration for the development loop.
//!
//! This module validates and preflights externally supplied services. It does
//! not provision them or execute any development stage.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use oci_client::Reference;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout_at};
use tokio_postgres::Config as PostgresConfig;
use url::Url;
use wamn_pg_core::Identifier;
use wamn_runtime::component_artifact::component_artifact_reference;

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

pub(super) const POSTGRES_SYSTEM_DATABASES: [&str; 3] = ["postgres", "template0", "template1"];
const POSTGRES_ROUTING_QUERY_KEYS: [&str; 5] = ["host", "hostaddr", "port", "dbname", "user"];

const CONFIG_KEYS: [&str; 18] = [
    VERIFICATION_DATABASE_URL,
    TARGET_DATABASE_URL,
    SYSTEM_DATABASE_URL,
    IDENTITY_DATABASE_URL,
    GUEST_DATABASE_URL,
    EXECUTOR_PLATFORM_DATABASE_URL,
    HTTP_ADMITTER_DATABASE_URL,
    EVENT_MATERIALIZER_DATABASE_URL,
    SCHEDULER_NATS_URL,
    EVENT_NATS_URL,
    COMPONENT_ARTIFACT_BASE,
    RELEASE_ARTIFACT_BASE,
    REGISTRY_AUTH_FILE,
    INSECURE_REGISTRY,
    GATE_URL,
    GATE_BEARER_TOKEN,
    ROUTE_HOST,
    FLOW_HTTP_WORKLOAD_IMAGE,
];

const REFERENCE_PROBE_DIGEST: &str =
    "sha256:0000000000000000000000000000000000000000000000000000000000000000";

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
    for key in object.keys() {
        if !CONFIG_KEYS.contains(&key.as_str()) {
            return Err(DevConfigError::new(
                DevConfigErrorKind::UnknownKey,
                key.as_str(),
                "remove the unknown key",
            ));
        }
    }

    let values = object
        .iter()
        .map(|(key, value)| (key.as_str(), value))
        .collect::<BTreeMap<_, _>>();
    let verification_database_url = required_string(&values, VERIFICATION_DATABASE_URL)?;
    let target_database_url = required_string(&values, TARGET_DATABASE_URL)?;
    let system_database_url = required_string(&values, SYSTEM_DATABASE_URL)?;
    let identity_database_url = required_string(&values, IDENTITY_DATABASE_URL)?;
    let guest_database_url = required_string(&values, GUEST_DATABASE_URL)?;
    let executor_platform_database_url = required_string(&values, EXECUTOR_PLATFORM_DATABASE_URL)?;
    let http_admitter_database_url = required_string(&values, HTTP_ADMITTER_DATABASE_URL)?;
    let event_materializer_database_url =
        required_string(&values, EVENT_MATERIALIZER_DATABASE_URL)?;
    let scheduler_nats_url = required_string(&values, SCHEDULER_NATS_URL)?;
    let event_nats_url = required_string(&values, EVENT_NATS_URL)?;
    let component_artifact_base = required_string(&values, COMPONENT_ARTIFACT_BASE)?;
    let release_artifact_base = required_string(&values, RELEASE_ARTIFACT_BASE)?;
    let registry_auth_file = required_string(&values, REGISTRY_AUTH_FILE)?;
    let insecure_registry = required_bool(&values, INSECURE_REGISTRY)?;
    let gate_url = required_string(&values, GATE_URL)?;
    let gate_bearer_token = required_string(&values, GATE_BEARER_TOKEN)?;
    let route_host = required_string(&values, ROUTE_HOST)?;
    let flow_http_workload_image = required_string(&values, FLOW_HTTP_WORKLOAD_IMAGE)?;

    validate_route_host(&route_host)?;
    let registry_auth_file = PathBuf::from(registry_auth_file.as_ref());

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

fn required_string(
    values: &BTreeMap<&str, &Value>,
    key: &'static str,
) -> Result<Box<str>, DevConfigError> {
    let value = values.get(key).ok_or_else(|| {
        DevConfigError::new(
            DevConfigErrorKind::MissingKey,
            key,
            "supply the required deployment value",
        )
    })?;
    let value = value
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            DevConfigError::new(
                DevConfigErrorKind::InvalidValue,
                key,
                "expected a non-empty string",
            )
        })?;
    Ok(value.into())
}

fn required_bool(
    values: &BTreeMap<&str, &Value>,
    key: &'static str,
) -> Result<bool, DevConfigError> {
    values
        .get(key)
        .ok_or_else(|| {
            DevConfigError::new(
                DevConfigErrorKind::MissingKey,
                key,
                "supply the required deployment value",
            )
        })?
        .as_bool()
        .ok_or_else(|| {
            DevConfigError::new(DevConfigErrorKind::InvalidValue, key, "expected a boolean")
        })
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

    use serde_json::json;
    use tokio::net::TcpListener;

    const ENDPOINT_COUNT: usize = 14;

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
