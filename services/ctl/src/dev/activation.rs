//! Native local-host activation for the development loop.
//!
//! Activation consumes caller-supplied infrastructure. It supervises one
//! `wamn-host` process and drives the upstream wash-runtime workload API; it
//! provisions nothing and owns no promotion verb.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;
use futures_util::StreamExt as _;
use oci_client::Reference;
use rustix::process::{Pid, Signal, kill_process};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::process::{Child, Command};
use tokio::time::{Instant, timeout, timeout_at};
use wamn_runtime::registry_credentials::{RegistryCredentials, read_registry_credentials};
use wash_runtime::washlet::{OPERATOR_API_PREFIX, rpc_subject, types::v2};

use super::config::DevConfig;
use crate::print_release_env::ReleaseCarrier;

/// Bound for connecting to the already-running development scheduler.
pub const SCHEDULER_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Bound for observing the exact local host's first heartbeat.
pub const HOST_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound for each native workload request and response.
pub const WORKLOAD_RPC_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound for graceful host shutdown before the kill-and-reap fallback.
pub const HOST_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

const FLOW_HTTP_NAME: &str = "flow-http";
const FLOW_HTTP_WORKLOAD_ID: &str = "wamn-dev-flow-http";
const LOOPBACK_HTTP_BIND: &str = "127.0.0.1:0";

/// Exact deployment identity carried into the local serving process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevActivationIdentity {
    pub tenant: String,
    pub catalog: String,
    pub environment: String,
    pub org: String,
    pub project: String,
    pub schema: String,
    pub host_group: String,
    pub host_name: String,
    pub runner: String,
}

/// Inputs produced by earlier stages and deployment-owned config.
pub struct DevActivationRequest<'a> {
    pub config: &'a DevConfig,
    pub release: &'a ReleaseCarrier,
    pub identity: &'a DevActivationIdentity,
    pub host_binary: &'a Path,
    pub wasmtime_cache_dir: &'a Path,
}

impl fmt::Debug for DevActivationRequest<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DevActivationRequest")
            .field("config", self.config)
            .field("release", self.release)
            .field("identity", self.identity)
            .field("host_binary", &self.host_binary)
            .field("wasmtime_cache_dir", &self.wasmtime_cache_dir)
            .finish()
    }
}

/// Stable category of an activation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevActivationErrorKind {
    InvalidInput,
    RegistryCredential,
    SchedulerUnavailable,
    HostProcess,
    HeartbeatUnavailable,
    ProtocolViolation,
    WorkloadRefused,
    CleanupFailed,
}

impl DevActivationErrorKind {
    /// Stable diagnostic code for this category.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "dev-activation-invalid-input",
            Self::RegistryCredential => "dev-activation-registry-credential",
            Self::SchedulerUnavailable => "dev-activation-scheduler-unavailable",
            Self::HostProcess => "dev-activation-host-process",
            Self::HeartbeatUnavailable => "dev-activation-heartbeat-unavailable",
            Self::ProtocolViolation => "dev-activation-protocol-violation",
            Self::WorkloadRefused => "dev-activation-workload-refused",
            Self::CleanupFailed => "dev-activation-cleanup-failed",
        }
    }
}

/// Contextual activation failure translated once at the development boundary.
#[derive(Debug)]
pub struct DevActivationError {
    kind: DevActivationErrorKind,
    step: &'static str,
    detail: Box<str>,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl DevActivationError {
    fn new(kind: DevActivationErrorKind, step: &'static str, detail: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            step,
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        kind: DevActivationErrorKind,
        step: &'static str,
        detail: impl Into<Box<str>>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            kind,
            step,
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Stable error category.
    pub const fn kind(&self) -> DevActivationErrorKind {
        self.kind
    }

    /// Exact activation step that failed.
    pub const fn step(&self) -> &'static str {
        self.step
    }

    /// Sanitized actionable detail.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for DevActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} at {}: {}",
            self.kind.as_str(),
            self.step,
            self.detail
        )
    }
}

impl Error for DevActivationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

#[derive(Clone, Eq, PartialEq)]
struct HostProcessSpec {
    program: PathBuf,
    args: Box<[String]>,
    env: Box<[(String, String)]>,
}

impl fmt::Debug for HostProcessSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostProcessSpec")
            .field("program", &self.program)
            .field("args", &self.args)
            .field(
                "env_keys",
                &self.env.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[derive(Debug)]
struct RuntimeMessage {
    subject: String,
    payload: Box<[u8]>,
}

trait ActivationBackend {
    type Error: Error + Send + Sync + 'static;

    fn subscribe_heartbeats(
        &mut self,
        subject: &str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn flush(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn spawn_host(
        &mut self,
        spec: &HostProcessSpec,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn next_heartbeat(
        &mut self,
        deadline: Instant,
    ) -> impl Future<Output = Result<Option<RuntimeMessage>, Self::Error>> + Send;

    fn request(
        &mut self,
        subject: &str,
        payload: &[u8],
        deadline: Instant,
    ) -> impl Future<Output = Result<Box<[u8]>, Self::Error>> + Send;

    fn terminate_host(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// `None` means the deadline expired; `Some` means the process was reaped.
    ///
    /// The boolean is the process exit status for diagnostics only. A process
    /// terminated by SIGTERM normally reports an unsuccessful exit status.
    fn wait_host(
        &mut self,
        deadline: Instant,
    ) -> impl Future<Output = Result<Option<bool>, Self::Error>> + Send;

    fn kill_host(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send;

    fn reap_host(
        &mut self,
        deadline: Instant,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

struct NativeActivationBackend {
    client: async_nats::Client,
    heartbeat: Option<async_nats::Subscriber>,
    child: Option<Child>,
}

#[derive(Debug)]
struct NativeBackendError(anyhow::Error);

impl fmt::Display for NativeBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl Error for NativeBackendError {}

impl From<anyhow::Error> for NativeBackendError {
    fn from(source: anyhow::Error) -> Self {
        Self(source)
    }
}

impl NativeActivationBackend {
    async fn connect(url: &str) -> Result<Self, DevActivationError> {
        let client = timeout(SCHEDULER_CONNECT_TIMEOUT, async_nats::connect(url))
            .await
            .map_err(|source| {
                DevActivationError::with_source(
                    DevActivationErrorKind::SchedulerUnavailable,
                    "connect-scheduler",
                    "scheduler connection deadline expired",
                    source,
                )
            })?
            .map_err(|source| {
                DevActivationError::with_source(
                    DevActivationErrorKind::SchedulerUnavailable,
                    "connect-scheduler",
                    "scheduler refused the NATS connection",
                    source,
                )
            })?;
        Ok(Self {
            client,
            heartbeat: None,
            child: None,
        })
    }

    fn child(&mut self) -> anyhow::Result<&mut Child> {
        self.child
            .as_mut()
            .context("local wamn-host process is not running")
    }
}

impl ActivationBackend for NativeActivationBackend {
    type Error = NativeBackendError;

    async fn subscribe_heartbeats(&mut self, subject: &str) -> Result<(), Self::Error> {
        let subscriber = self
            .client
            .subscribe(subject.to_owned())
            .await
            .context("subscribe to native host heartbeats")?;
        self.heartbeat = Some(subscriber);
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(self
            .client
            .flush()
            .await
            .context("flush native host heartbeat subscription")?)
    }

    async fn spawn_host(&mut self, spec: &HostProcessSpec) -> Result<(), Self::Error> {
        let mut command = Command::new(&spec.program);
        command
            .args(spec.args.iter())
            .env_clear()
            .envs(spec.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        self.child = Some(
            command
                .spawn()
                .with_context(|| format!("spawn local wamn-host at {}", spec.program.display()))?,
        );
        Ok(())
    }

    async fn next_heartbeat(
        &mut self,
        deadline: Instant,
    ) -> Result<Option<RuntimeMessage>, Self::Error> {
        let subscriber = self
            .heartbeat
            .as_mut()
            .context("heartbeat subscription was not prepared")?;
        let message = timeout_at(deadline, subscriber.next())
            .await
            .context("native host heartbeat deadline expired")?;
        Ok(message.map(|message| RuntimeMessage {
            subject: message.subject.to_string(),
            payload: message.payload.to_vec().into_boxed_slice(),
        }))
    }

    async fn request(
        &mut self,
        subject: &str,
        payload: &[u8],
        deadline: Instant,
    ) -> Result<Box<[u8]>, Self::Error> {
        let response = timeout_at(
            deadline,
            self.client
                .request(subject.to_owned(), payload.to_vec().into()),
        )
        .await
        .context("native workload request deadline expired")?
        .context("native workload request failed")?;
        Ok(response.payload.to_vec().into_boxed_slice())
    }

    async fn terminate_host(&mut self) -> Result<(), Self::Error> {
        let Some(raw_pid) = self.child()?.id() else {
            return Ok(());
        };
        let pid = i32::try_from(raw_pid)
            .ok()
            .and_then(Pid::from_raw)
            .context("local wamn-host PID is outside the platform range")?;
        Ok(kill_process(pid, Signal::TERM).context("send SIGTERM to local wamn-host")?)
    }

    async fn wait_host(&mut self, deadline: Instant) -> Result<Option<bool>, Self::Error> {
        match timeout_at(deadline, self.child()?.wait()).await {
            Ok(status) => Ok(Some(status.context("wait for local wamn-host")?.success())),
            Err(_) => Ok(None),
        }
    }

    async fn kill_host(&mut self) -> Result<(), Self::Error> {
        Ok(self
            .child()?
            .start_kill()
            .context("kill unresponsive local wamn-host")?)
    }

    async fn reap_host(&mut self, deadline: Instant) -> Result<(), Self::Error> {
        timeout_at(deadline, self.child()?.wait())
            .await
            .context("reap local wamn-host deadline expired")?
            .context("reap local wamn-host")?;
        Ok(())
    }
}

struct BackendActivation<B> {
    backend: B,
    host_id: Box<str>,
    workload_id: Box<str>,
}

/// Live local host and flow-http workload.
///
/// Call [`shutdown`](Self::shutdown) to complete native workload stop and
/// bounded process reaping. Dropping without shutdown still kills the child
/// through Tokio's `kill_on_drop` safety net.
pub struct DevActivation {
    active: BackendActivation<NativeActivationBackend>,
}

impl fmt::Debug for DevActivation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DevActivation")
            .field("host_id", &self.active.host_id)
            .field("workload_id", &self.active.workload_id)
            .finish_non_exhaustive()
    }
}

impl DevActivation {
    /// Runtime-assigned host identity selected from the exact heartbeat.
    pub fn host_id(&self) -> &str {
        &self.active.host_id
    }

    /// Stable local flow-http workload identity.
    pub fn workload_id(&self) -> &str {
        &self.active.workload_id
    }

    /// Stop the workload, then terminate and reap the supervised host.
    pub async fn shutdown(self) -> Result<(), DevActivationError> {
        let BackendActivation {
            mut backend,
            host_id,
            workload_id,
        } = self.active;
        shutdown_backend(&mut backend, &host_id, &workload_id).await
    }
}

/// Start one local host and its flow-http workload through native NATS.
pub async fn activate(
    request: DevActivationRequest<'_>,
) -> Result<DevActivation, DevActivationError> {
    validate_request(&request)?;
    let image =
        Reference::try_from(request.config.flow_http_workload_image()).map_err(|source| {
            DevActivationError::with_source(
                DevActivationErrorKind::InvalidInput,
                "workload-image",
                "configured flow-http workload image is malformed",
                source,
            )
        })?;
    let credentials =
        read_registry_credentials(request.config.registry_auth_file(), image.registry()).map_err(
            |source| {
                DevActivationError::with_source(
                    DevActivationErrorKind::RegistryCredential,
                    "workload-image-credential",
                    "load the exact flow-http registry credential",
                    source,
                )
            },
        )?;
    let pull_secret = image_pull_secret(&credentials);
    let backend = NativeActivationBackend::connect(request.config.scheduler_nats_url()).await?;
    let active = activate_backend(request, pull_secret, backend).await?;
    Ok(DevActivation { active })
}

fn validate_request(request: &DevActivationRequest<'_>) -> Result<(), DevActivationError> {
    let identity = request.identity;
    for (field, value) in [
        ("tenant", identity.tenant.as_str()),
        ("catalog", identity.catalog.as_str()),
        ("environment", identity.environment.as_str()),
        ("org", identity.org.as_str()),
        ("project", identity.project.as_str()),
        ("schema", identity.schema.as_str()),
        ("host-group", identity.host_group.as_str()),
        ("host-name", identity.host_name.as_str()),
        ("runner", identity.runner.as_str()),
    ] {
        if value.is_empty() {
            return Err(DevActivationError::new(
                DevActivationErrorKind::InvalidInput,
                "validate-identity",
                format!("supply the required activation identity {field}"),
            ));
        }
    }
    if request.release.artifact_base.is_empty() {
        return Err(DevActivationError::new(
            DevActivationErrorKind::InvalidInput,
            "validate-release",
            "supply the release artifact base",
        ));
    }
    if request.host_binary.as_os_str().is_empty() {
        return Err(DevActivationError::new(
            DevActivationErrorKind::InvalidInput,
            "validate-host",
            "supply the wamn-host executable path",
        ));
    }
    if request.wasmtime_cache_dir.as_os_str().is_empty() {
        return Err(DevActivationError::new(
            DevActivationErrorKind::InvalidInput,
            "validate-host",
            "supply the Wasmtime cache directory",
        ));
    }
    Ok(())
}

fn image_pull_secret(credentials: &RegistryCredentials) -> v2::ImagePullSecret {
    v2::ImagePullSecret {
        username: credentials.username().to_owned(),
        password: credentials.password().to_owned(),
    }
}

fn host_process_spec(request: &DevActivationRequest<'_>) -> HostProcessSpec {
    let identity = request.identity;
    let mut args = vec![
        "host".to_owned(),
        "--host-group".to_owned(),
        identity.host_group.clone(),
        "--scheduler-nats-url".to_owned(),
        request.config.scheduler_nats_url().to_owned(),
        "--host-name".to_owned(),
        identity.host_name.clone(),
        "--runner".to_owned(),
        identity.runner.clone(),
        "--environment".to_owned(),
        identity.environment.clone(),
        "--http-addr".to_owned(),
        LOOPBACK_HTTP_BIND.to_owned(),
        "--release-artifact-base".to_owned(),
        request.release.artifact_base.clone(),
        "--release-manifest-digest".to_owned(),
        request.release.manifest_digest.to_string(),
        "--component-artifact-base".to_owned(),
        request.config.component_artifact_base().to_owned(),
        "--registry-auth-file".to_owned(),
        request.config.registry_auth_file().display().to_string(),
        "--wasmtime-cache-dir".to_owned(),
        request.wasmtime_cache_dir.display().to_string(),
        "--project".to_owned(),
        identity.project.clone(),
        "--org".to_owned(),
        identity.org.clone(),
        "--schema".to_owned(),
        identity.schema.clone(),
    ];
    if request.config.insecure_registry() {
        args.push("--allow-insecure-registries".to_owned());
    }
    let env = vec![
        (
            "WAMN_SYSTEM_URL".to_owned(),
            request.config.identity_database_url().to_owned(),
        ),
        (
            "WAMN_PG_URL".to_owned(),
            request.config.guest_database_url().to_owned(),
        ),
        (
            "WAMN_EXECUTOR_PLATFORM_PG_URL".to_owned(),
            request.config.executor_platform_database_url().to_owned(),
        ),
        (
            "WAMN_HTTP_ADMITTER_PG_URL".to_owned(),
            request.config.http_admitter_database_url().to_owned(),
        ),
        (
            "WAMN_EVENT_MATERIALIZER_PG_URL".to_owned(),
            request.config.event_materializer_database_url().to_owned(),
        ),
        (
            "WAMN_EVT_NATS_URL".to_owned(),
            request.config.event_nats_url().to_owned(),
        ),
    ];
    HostProcessSpec {
        program: request.host_binary.to_owned(),
        args: args.into_boxed_slice(),
        env: env.into_boxed_slice(),
    }
}

fn flow_http_request(
    request: &DevActivationRequest<'_>,
    pull_secret: v2::ImagePullSecret,
) -> v2::WorkloadStartRequest {
    let identity = request.identity;
    let config = HashMap::from([
        ("wamn.tenant".to_owned(), identity.tenant.clone()),
        ("wamn.catalog".to_owned(), identity.catalog.clone()),
        ("wamn.environment".to_owned(), identity.environment.clone()),
        ("wamn.project".to_owned(), identity.project.clone()),
        ("wamn.schema".to_owned(), identity.schema.clone()),
    ]);
    let local_resources = v2::LocalResources {
        memory_limit_mb: 0,
        cpu_limit: 0,
        config,
        environment: HashMap::new(),
        volume_mounts: Vec::new(),
        allowed_hosts: Vec::new(),
        allowed_ip_name_lookups: Vec::new(),
        allowed_host_loopback_ports: Vec::new(),
    };
    v2::WorkloadStartRequest {
        workload_id: FLOW_HTTP_WORKLOAD_ID.to_owned(),
        workload: Some(v2::Workload {
            namespace: identity.environment.clone(),
            name: FLOW_HTTP_NAME.to_owned(),
            annotations: HashMap::new(),
            service: None,
            wit_world: Some(v2::WitWorld {
                components: vec![v2::Component {
                    image: request.config.flow_http_workload_image().to_owned(),
                    local_resources: Some(local_resources),
                    pool_size: 0,
                    max_invocations: 0,
                    image_pull_secret: Some(pull_secret),
                    name: FLOW_HTTP_NAME.to_owned(),
                    image_pull_policy: v2::ImagePullPolicy::Always.into(),
                    max_concurrency: 0,
                }],
                host_interfaces: vec![
                    wit_interface("wasi", "http", "", "incoming-handler"),
                    wit_interface("wamn", "flow-http-routing", "0.1.0", "routing"),
                    wit_interface("wamn", "router-delivery", "0.1.0", "delivery"),
                ],
            }),
            volumes: Vec::new(),
        }),
    }
}

fn wit_interface(
    namespace: &str,
    package: &str,
    version: &str,
    interface: &str,
) -> v2::WitInterface {
    v2::WitInterface {
        namespace: namespace.to_owned(),
        package: package.to_owned(),
        version: version.to_owned(),
        interfaces: vec![interface.to_owned()],
        config: HashMap::new(),
        name: String::new(),
    }
}

async fn activate_backend<B>(
    request: DevActivationRequest<'_>,
    pull_secret: v2::ImagePullSecret,
    mut backend: B,
) -> Result<BackendActivation<B>, DevActivationError>
where
    B: ActivationBackend,
{
    validate_request(&request)?;
    let heartbeat_subject = format!("{OPERATOR_API_PREFIX}.heartbeat.*");
    backend
        .subscribe_heartbeats(&heartbeat_subject)
        .await
        .map_err(|source| backend_error("subscribe-heartbeats", source))?;
    backend
        .flush()
        .await
        .map_err(|source| backend_error("flush-heartbeats", source))?;
    let spec = host_process_spec(&request);
    backend.spawn_host(&spec).await.map_err(|source| {
        DevActivationError::with_source(
            DevActivationErrorKind::HostProcess,
            "spawn-host",
            "local wamn-host could not start",
            source,
        )
    })?;

    let mut host_id = None;
    let activation = async {
        let selected = wait_for_host(&mut backend, request.identity).await?;
        host_id = Some(selected);
        let selected = host_id
            .as_deref()
            .expect("selected host identity is assigned before workload start");
        let start = flow_http_request(&request, pull_secret);
        let start_response: v2::WorkloadStartResponse = request_json(
            &mut backend,
            &rpc_subject(selected, "workload.start"),
            &start,
            "start-workload",
        )
        .await?;
        require_running(
            "start-workload",
            start_response.workload_status,
            FLOW_HTTP_WORKLOAD_ID,
        )?;

        let status_response: v2::WorkloadStatusResponse = request_json(
            &mut backend,
            &rpc_subject(selected, "workload.status"),
            &v2::WorkloadStatusRequest {
                workload_id: FLOW_HTTP_WORKLOAD_ID.to_owned(),
            },
            "status-workload",
        )
        .await?;
        require_running(
            "status-workload",
            status_response.workload_status,
            FLOW_HTTP_WORKLOAD_ID,
        )?;
        Ok(())
    }
    .await;

    if let Err(error) = activation {
        if let Err(cleanup) = shutdown_backend(
            &mut backend,
            host_id.as_deref().unwrap_or_default(),
            FLOW_HTTP_WORKLOAD_ID,
        )
        .await
        {
            tracing::warn!(error = %cleanup, "activation cleanup also failed");
        }
        return Err(error);
    }

    Ok(BackendActivation {
        backend,
        host_id: host_id
            .expect("successful activation selected one host")
            .into_boxed_str(),
        workload_id: FLOW_HTTP_WORKLOAD_ID.into(),
    })
}

async fn wait_for_host<B>(
    backend: &mut B,
    identity: &DevActivationIdentity,
) -> Result<String, DevActivationError>
where
    B: ActivationBackend,
{
    let deadline = Instant::now() + HOST_HEARTBEAT_TIMEOUT;
    loop {
        let message = backend
            .next_heartbeat(deadline)
            .await
            .map_err(|source| {
                DevActivationError::with_source(
                    DevActivationErrorKind::HeartbeatUnavailable,
                    "wait-heartbeat",
                    "the exact local host did not publish before the deadline",
                    source,
                )
            })?
            .ok_or_else(|| {
                DevActivationError::new(
                    DevActivationErrorKind::HeartbeatUnavailable,
                    "wait-heartbeat",
                    "the native heartbeat subscription closed",
                )
            })?;
        let heartbeat: v2::HostHeartbeat = decode_json(
            &message.payload,
            "wait-heartbeat",
            "heartbeat payload is not the pinned native v2 shape",
        )?;
        if heartbeat_matches(&message.subject, &heartbeat, identity) {
            return Ok(heartbeat.id);
        }
    }
}

fn heartbeat_matches(
    subject: &str,
    heartbeat: &v2::HostHeartbeat,
    identity: &DevActivationIdentity,
) -> bool {
    let prefix = format!("{OPERATOR_API_PREFIX}.heartbeat.");
    let Some(subject_host_id) = subject.strip_prefix(&prefix) else {
        return false;
    };
    !subject_host_id.is_empty()
        && !subject_host_id.contains('.')
        && subject_host_id == heartbeat.id
        && heartbeat.hostname == identity.host_name
        && heartbeat.environment == identity.environment
        && heartbeat.labels.get("hostgroup") == Some(&identity.host_group)
}

async fn request_json<B, Req, Resp>(
    backend: &mut B,
    subject: &str,
    request: &Req,
    step: &'static str,
) -> Result<Resp, DevActivationError>
where
    B: ActivationBackend,
    Req: Serialize,
    Resp: DeserializeOwned,
{
    let payload = serde_json::to_vec(request).map_err(|source| {
        DevActivationError::with_source(
            DevActivationErrorKind::ProtocolViolation,
            step,
            "encode the pinned native v2 request",
            source,
        )
    })?;
    let response = backend
        .request(subject, &payload, Instant::now() + WORKLOAD_RPC_TIMEOUT)
        .await
        .map_err(|source| backend_error(step, source))?;
    decode_json(
        &response,
        step,
        "response is not the pinned native v2 shape",
    )
}

fn decode_json<T>(
    payload: &[u8],
    step: &'static str,
    detail: &'static str,
) -> Result<T, DevActivationError>
where
    T: DeserializeOwned,
{
    serde_json::from_slice(payload).map_err(|source| {
        DevActivationError::with_source(
            DevActivationErrorKind::ProtocolViolation,
            step,
            detail,
            source,
        )
    })
}

fn require_running(
    step: &'static str,
    status: Option<v2::WorkloadStatus>,
    workload_id: &str,
) -> Result<(), DevActivationError> {
    let status = status.ok_or_else(|| {
        DevActivationError::new(
            DevActivationErrorKind::ProtocolViolation,
            step,
            "response omitted workload_status",
        )
    })?;
    if status.workload_id != workload_id {
        return Err(DevActivationError::new(
            DevActivationErrorKind::ProtocolViolation,
            step,
            format!(
                "response named workload {:?}, expected {workload_id:?}",
                status.workload_id
            ),
        ));
    }
    let state = v2::WorkloadState::try_from(status.workload_state).map_err(|_| {
        DevActivationError::new(
            DevActivationErrorKind::ProtocolViolation,
            step,
            format!(
                "response carried unknown workload state {}",
                status.workload_state
            ),
        )
    })?;
    if state != v2::WorkloadState::Running {
        return Err(DevActivationError::new(
            DevActivationErrorKind::WorkloadRefused,
            step,
            format!(
                "{workload_id} reached {}: {}",
                state.as_str_name(),
                status.message
            ),
        ));
    }
    Ok(())
}

async fn shutdown_backend<B>(
    backend: &mut B,
    host_id: &str,
    workload_id: &str,
) -> Result<(), DevActivationError>
where
    B: ActivationBackend,
{
    let mut first_error = None;
    if !host_id.is_empty() {
        let stop: Result<v2::WorkloadStopResponse, DevActivationError> = request_json(
            backend,
            &rpc_subject(host_id, "workload.stop"),
            &v2::WorkloadStopRequest {
                workload_id: workload_id.to_owned(),
            },
            "stop-workload",
        )
        .await;
        match stop.and_then(|response| require_stopped(response.workload_status, workload_id)) {
            Ok(()) => {}
            Err(error) => first_error = Some(error),
        }
    }

    if let Err(source) = backend.terminate_host().await {
        first_error.get_or_insert_with(|| {
            DevActivationError::with_source(
                DevActivationErrorKind::CleanupFailed,
                "terminate-host",
                "send SIGTERM to the local host",
                source,
            )
        });
    }

    let graceful = match backend
        .wait_host(Instant::now() + HOST_SHUTDOWN_TIMEOUT)
        .await
    {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(source) => {
            first_error.get_or_insert_with(|| {
                DevActivationError::with_source(
                    DevActivationErrorKind::CleanupFailed,
                    "wait-host",
                    "wait for local host after SIGTERM",
                    source,
                )
            });
            false
        }
    };

    if !graceful {
        if let Err(source) = backend.kill_host().await {
            first_error.get_or_insert_with(|| {
                DevActivationError::with_source(
                    DevActivationErrorKind::CleanupFailed,
                    "kill-host",
                    "kill the unresponsive local host",
                    source,
                )
            });
        }
        if let Err(source) = backend
            .reap_host(Instant::now() + HOST_SHUTDOWN_TIMEOUT)
            .await
        {
            first_error.get_or_insert_with(|| {
                DevActivationError::with_source(
                    DevActivationErrorKind::CleanupFailed,
                    "reap-host",
                    "reap the killed local host before the deadline",
                    source,
                )
            });
        }
    }

    first_error.map_or(Ok(()), Err)
}

fn require_stopped(
    status: Option<v2::WorkloadStatus>,
    workload_id: &str,
) -> Result<(), DevActivationError> {
    let status = status.ok_or_else(|| {
        DevActivationError::new(
            DevActivationErrorKind::ProtocolViolation,
            "stop-workload",
            "response omitted workload_status",
        )
    })?;
    if status.workload_id != workload_id {
        return Err(DevActivationError::new(
            DevActivationErrorKind::ProtocolViolation,
            "stop-workload",
            "response named a different workload",
        ));
    }
    let state = v2::WorkloadState::try_from(status.workload_state).map_err(|_| {
        DevActivationError::new(
            DevActivationErrorKind::ProtocolViolation,
            "stop-workload",
            "response carried an unknown workload state",
        )
    })?;
    if matches!(
        state,
        v2::WorkloadState::Stopping | v2::WorkloadState::NotFound
    ) {
        Ok(())
    } else {
        Err(DevActivationError::new(
            DevActivationErrorKind::CleanupFailed,
            "stop-workload",
            format!("workload stop reached {}", state.as_str_name()),
        ))
    }
}

fn backend_error(
    step: &'static str,
    source: impl Error + Send + Sync + 'static,
) -> DevActivationError {
    DevActivationError::with_source(
        DevActivationErrorKind::SchedulerUnavailable,
        step,
        "native runtime NATS operation failed",
        source,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use wamn_catalog::ManifestDigest;

    use super::*;
    use crate::dev::config::parse_config;

    #[derive(Clone, Debug, Eq, PartialEq)]
    enum Event {
        Subscribe(String),
        Flush,
        Spawn,
        Heartbeat,
        Request(String),
        Terminate,
        Wait,
        Kill,
        Reap,
    }

    #[derive(Debug)]
    struct FakeError(&'static str);

    impl fmt::Display for FakeError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.0)
        }
    }

    impl Error for FakeError {}

    #[derive(Default)]
    struct Shared {
        events: Mutex<Vec<Event>>,
        spec: Mutex<Option<HostProcessSpec>>,
        requests: Mutex<Vec<(String, Box<[u8]>)>>,
    }

    struct FakeBackend {
        shared: Arc<Shared>,
        heartbeats: VecDeque<RuntimeMessage>,
        responses: VecDeque<Box<[u8]>>,
        waits: VecDeque<Option<bool>>,
    }

    impl FakeBackend {
        fn new(
            shared: Arc<Shared>,
            heartbeats: impl IntoIterator<Item = RuntimeMessage>,
            responses: impl IntoIterator<Item = Box<[u8]>>,
            waits: impl IntoIterator<Item = Option<bool>>,
        ) -> Self {
            Self {
                shared,
                heartbeats: heartbeats.into_iter().collect(),
                responses: responses.into_iter().collect(),
                waits: waits.into_iter().collect(),
            }
        }

        fn record(&self, event: Event) {
            self.shared
                .events
                .lock()
                .expect("event lock is not poisoned")
                .push(event);
        }
    }

    impl ActivationBackend for FakeBackend {
        type Error = FakeError;

        async fn subscribe_heartbeats(&mut self, subject: &str) -> Result<(), Self::Error> {
            self.record(Event::Subscribe(subject.to_owned()));
            Ok(())
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            self.record(Event::Flush);
            Ok(())
        }

        async fn spawn_host(&mut self, spec: &HostProcessSpec) -> Result<(), Self::Error> {
            self.record(Event::Spawn);
            *self
                .shared
                .spec
                .lock()
                .expect("process-spec lock is not poisoned") = Some(spec.clone());
            Ok(())
        }

        async fn next_heartbeat(
            &mut self,
            _deadline: Instant,
        ) -> Result<Option<RuntimeMessage>, Self::Error> {
            self.record(Event::Heartbeat);
            Ok(self.heartbeats.pop_front())
        }

        async fn request(
            &mut self,
            subject: &str,
            payload: &[u8],
            _deadline: Instant,
        ) -> Result<Box<[u8]>, Self::Error> {
            self.record(Event::Request(subject.to_owned()));
            self.shared
                .requests
                .lock()
                .expect("request lock is not poisoned")
                .push((subject.to_owned(), payload.into()));
            self.responses
                .pop_front()
                .ok_or(FakeError("no response configured"))
        }

        async fn terminate_host(&mut self) -> Result<(), Self::Error> {
            self.record(Event::Terminate);
            Ok(())
        }

        async fn wait_host(&mut self, _deadline: Instant) -> Result<Option<bool>, Self::Error> {
            self.record(Event::Wait);
            self.waits
                .pop_front()
                .ok_or(FakeError("no wait outcome configured"))
        }

        async fn kill_host(&mut self) -> Result<(), Self::Error> {
            self.record(Event::Kill);
            Ok(())
        }

        async fn reap_host(&mut self, _deadline: Instant) -> Result<(), Self::Error> {
            self.record(Event::Reap);
            Ok(())
        }
    }

    fn config() -> DevConfig {
        parse_config(
            &serde_json::to_vec(&json!({
                "verification_database_url": "postgresql://verify:verify-secret@127.0.0.1:41001/verification",
                "target_database_url": "postgresql://target:target-secret@127.0.0.1:41002/target",
                "system_database_url": "postgresql://owner:owner-secret@127.0.0.1:41003/system",
                "identity_database_url": "postgresql://identity:identity-secret@127.0.0.1:41004/system",
                "guest_database_url": "postgresql://guest:guest-secret@127.0.0.1:41005/target",
                "executor_platform_database_url": "postgresql://platform:platform-secret@127.0.0.1:41006/target",
                "http_admitter_database_url": "postgresql://admitter:admitter-secret@127.0.0.1:41007/target",
                "event_materializer_database_url": "postgresql://materializer:materializer-secret@127.0.0.1:41008/target",
                "scheduler_nats_url": "nats://127.0.0.1:41009",
                "event_nats_url": "nats://127.0.0.1:41010",
                "component_artifact_base": "127.0.0.1:41011/wamn/components",
                "release_artifact_base": "127.0.0.1:41012/wamn/releases",
                "registry_auth_file": "/run/secrets/dev-registry.json",
                "insecure_registry": true,
                "gate_url": "http://127.0.0.1:41013/authoring",
                "gate_bearer_token": "gate-secret",
                "route_host": "receiving.localhost",
                "flow_http_workload_image": "127.0.0.1:41014/wamn/flow-http:dev"
            }))
            .expect("serialize complete activation config"),
        )
        .expect("complete activation config parses")
    }

    fn identity() -> DevActivationIdentity {
        DevActivationIdentity {
            tenant: "00000000-0000-0000-0000-000000000001".to_owned(),
            catalog: "default".to_owned(),
            environment: "receiving-dev".to_owned(),
            org: "acme".to_owned(),
            project: "receiving".to_owned(),
            schema: "receiving".to_owned(),
            host_group: "wamn-dev-receiving".to_owned(),
            host_name: "wamn-dev-receiving-1".to_owned(),
            runner: "wamn-dev-receiving-1".to_owned(),
        }
    }

    fn release() -> ReleaseCarrier {
        ReleaseCarrier {
            artifact_base: "127.0.0.1:41012/wamn/releases".to_owned(),
            manifest_digest: ManifestDigest::parse(format!("sha256:{}", "7".repeat(64)))
                .expect("fixture digest is canonical"),
        }
    }

    fn pull_secret() -> v2::ImagePullSecret {
        v2::ImagePullSecret {
            username: "flow-reader".to_owned(),
            password: "flow-secret".to_owned(),
        }
    }

    fn heartbeat(id: &str, hostname: &str, environment: &str, host_group: &str) -> RuntimeMessage {
        let heartbeat = v2::HostHeartbeat {
            id: id.to_owned(),
            hostname: hostname.to_owned(),
            friendly_name: String::new(),
            version: "2.8.0".to_owned(),
            labels: HashMap::from([("hostgroup".to_owned(), host_group.to_owned())]),
            started_at: None,
            os_arch: String::new(),
            os_name: String::new(),
            os_kernel: String::new(),
            system_cpu_usage: 0.0,
            system_memory_total: 0,
            system_memory_free: 0,
            component_count: 0,
            workload_count: 0,
            imports: Vec::new(),
            exports: Vec::new(),
            http_port: 0,
            environment: environment.to_owned(),
        };
        RuntimeMessage {
            subject: format!("{OPERATOR_API_PREFIX}.heartbeat.{id}"),
            payload: serde_json::to_vec(&heartbeat)
                .expect("serialize heartbeat")
                .into_boxed_slice(),
        }
    }

    fn response<T: Serialize>(response: &T) -> Box<[u8]> {
        serde_json::to_vec(response)
            .expect("serialize native response")
            .into_boxed_slice()
    }

    fn status(state: v2::WorkloadState) -> v2::WorkloadStatus {
        v2::WorkloadStatus {
            workload_id: FLOW_HTTP_WORKLOAD_ID.to_owned(),
            workload_state: state.into(),
            message: state.as_str_name().to_owned(),
        }
    }

    #[tokio::test]
    async fn activation_uses_exact_native_sequence_and_bounded_cleanup() {
        let config = config();
        let identity = identity();
        let release = release();
        let request = DevActivationRequest {
            config: &config,
            release: &release,
            identity: &identity,
            host_binary: Path::new("/opt/wamn/bin/wamn-host"),
            wasmtime_cache_dir: Path::new("/tmp/wamn-dev-cache"),
        };
        let shared = Arc::new(Shared::default());
        let backend = FakeBackend::new(
            Arc::clone(&shared),
            [
                heartbeat(
                    "other",
                    "other-host",
                    &identity.environment,
                    &identity.host_group,
                ),
                heartbeat(
                    "selected-host-id",
                    &identity.host_name,
                    &identity.environment,
                    &identity.host_group,
                ),
            ],
            [
                response(&v2::WorkloadStartResponse {
                    workload_status: Some(status(v2::WorkloadState::Running)),
                }),
                response(&v2::WorkloadStatusResponse {
                    workload_status: Some(status(v2::WorkloadState::Running)),
                }),
                response(&v2::WorkloadStopResponse {
                    workload_status: Some(status(v2::WorkloadState::Stopping)),
                }),
            ],
            [None],
        );
        let expected_workload = flow_http_request(&request, pull_secret());

        let mut active = activate_backend(request, pull_secret(), backend)
            .await
            .expect("the exact local host activates");
        assert_eq!(&*active.host_id, "selected-host-id");
        shutdown_backend(&mut active.backend, &active.host_id, &active.workload_id)
            .await
            .expect("timeout falls back to kill and bounded reap");

        let spec = shared
            .spec
            .lock()
            .expect("process-spec lock is not poisoned")
            .clone()
            .expect("one host was spawned");
        assert_eq!(spec.program, Path::new("/opt/wamn/bin/wamn-host"));
        let expected_digest = format!("sha256:{}", "7".repeat(64));
        let expected_args = [
            "host",
            "--host-group",
            "wamn-dev-receiving",
            "--scheduler-nats-url",
            "nats://127.0.0.1:41009",
            "--host-name",
            "wamn-dev-receiving-1",
            "--runner",
            "wamn-dev-receiving-1",
            "--environment",
            "receiving-dev",
            "--http-addr",
            "127.0.0.1:0",
            "--release-artifact-base",
            "127.0.0.1:41012/wamn/releases",
            "--release-manifest-digest",
            expected_digest.as_str(),
            "--component-artifact-base",
            "127.0.0.1:41011/wamn/components",
            "--registry-auth-file",
            "/run/secrets/dev-registry.json",
            "--wasmtime-cache-dir",
            "/tmp/wamn-dev-cache",
            "--project",
            "receiving",
            "--org",
            "acme",
            "--schema",
            "receiving",
            "--allow-insecure-registries",
        ]
        .map(str::to_owned);
        assert_eq!(&*spec.args, &expected_args);
        assert_eq!(
            &*spec.env,
            &[
                (
                    "WAMN_SYSTEM_URL".to_owned(),
                    "postgresql://identity:identity-secret@127.0.0.1:41004/system".to_owned(),
                ),
                (
                    "WAMN_PG_URL".to_owned(),
                    "postgresql://guest:guest-secret@127.0.0.1:41005/target".to_owned(),
                ),
                (
                    "WAMN_EXECUTOR_PLATFORM_PG_URL".to_owned(),
                    "postgresql://platform:platform-secret@127.0.0.1:41006/target".to_owned(),
                ),
                (
                    "WAMN_HTTP_ADMITTER_PG_URL".to_owned(),
                    "postgresql://admitter:admitter-secret@127.0.0.1:41007/target".to_owned(),
                ),
                (
                    "WAMN_EVENT_MATERIALIZER_PG_URL".to_owned(),
                    "postgresql://materializer:materializer-secret@127.0.0.1:41008/target"
                        .to_owned(),
                ),
                (
                    "WAMN_EVT_NATS_URL".to_owned(),
                    "nats://127.0.0.1:41010".to_owned(),
                ),
            ]
        );

        let requests = shared
            .requests
            .lock()
            .expect("request lock is not poisoned");
        assert_eq!(requests.len(), 3, "start, one status receipt, then stop");
        assert_eq!(
            requests
                .iter()
                .map(|(subject, _)| subject.as_str())
                .collect::<Vec<_>>(),
            [
                "runtime.host.selected-host-id.workload.start",
                "runtime.host.selected-host-id.workload.status",
                "runtime.host.selected-host-id.workload.stop",
            ]
        );
        let actual_workload: v2::WorkloadStartRequest =
            serde_json::from_slice(&requests[0].1).expect("decode recorded start request");
        assert_eq!(actual_workload, expected_workload);
        drop(requests);

        assert_eq!(
            *shared.events.lock().expect("event lock is not poisoned"),
            vec![
                Event::Subscribe(format!("{OPERATOR_API_PREFIX}.heartbeat.*")),
                Event::Flush,
                Event::Spawn,
                Event::Heartbeat,
                Event::Heartbeat,
                Event::Request("runtime.host.selected-host-id.workload.start".to_owned()),
                Event::Request("runtime.host.selected-host-id.workload.status".to_owned()),
                Event::Request("runtime.host.selected-host-id.workload.stop".to_owned()),
                Event::Terminate,
                Event::Wait,
                Event::Kill,
                Event::Reap,
            ]
        );
    }

    #[tokio::test]
    async fn shutdown_accepts_sigterm_exit_without_kill_fallback() {
        let shared = Arc::new(Shared::default());
        let mut backend = FakeBackend::new(
            Arc::clone(&shared),
            [],
            [response(&v2::WorkloadStopResponse {
                workload_status: Some(status(v2::WorkloadState::Stopping)),
            })],
            [Some(false)],
        );

        shutdown_backend(&mut backend, "selected-host-id", FLOW_HTTP_WORKLOAD_ID)
            .await
            .expect("SIGTERM process status still means bounded cleanup completed");

        assert_eq!(
            *shared.events.lock().expect("event lock is not poisoned"),
            vec![
                Event::Request("runtime.host.selected-host-id.workload.stop".to_owned()),
                Event::Terminate,
                Event::Wait,
            ]
        );
    }

    #[tokio::test]
    async fn terminal_start_states_fail_before_status_and_still_cleanup() {
        for terminal in [
            v2::WorkloadState::Completed,
            v2::WorkloadState::Stopping,
            v2::WorkloadState::Error,
            v2::WorkloadState::NotFound,
        ] {
            let config = config();
            let identity = identity();
            let release = release();
            let request = DevActivationRequest {
                config: &config,
                release: &release,
                identity: &identity,
                host_binary: Path::new("/opt/wamn/bin/wamn-host"),
                wasmtime_cache_dir: Path::new("/tmp/wamn-dev-cache"),
            };
            let shared = Arc::new(Shared::default());
            let backend = FakeBackend::new(
                Arc::clone(&shared),
                [heartbeat(
                    "selected-host-id",
                    &identity.host_name,
                    &identity.environment,
                    &identity.host_group,
                )],
                [
                    response(&v2::WorkloadStartResponse {
                        workload_status: Some(status(terminal)),
                    }),
                    response(&v2::WorkloadStopResponse {
                        workload_status: Some(status(v2::WorkloadState::NotFound)),
                    }),
                ],
                [Some(true)],
            );

            let error = match activate_backend(request, pull_secret(), backend).await {
                Ok(_) => panic!("a terminal start response must fail fast"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), DevActivationErrorKind::WorkloadRefused);
            assert_eq!(error.step(), "start-workload");
            assert!(error.detail().contains(terminal.as_str_name()));
            let requests = shared
                .requests
                .lock()
                .expect("request lock is not poisoned");
            assert_eq!(requests.len(), 2, "start and cleanup stop only");
            assert!(requests[0].0.ends_with("workload.start"));
            assert!(requests[1].0.ends_with("workload.stop"));
        }
    }

    #[tokio::test]
    async fn invalid_identity_refuses_before_subscription_or_process_spawn() {
        let config = config();
        let mut identity = identity();
        identity.org.clear();
        let release = release();
        let request = DevActivationRequest {
            config: &config,
            release: &release,
            identity: &identity,
            host_binary: Path::new("/opt/wamn/bin/wamn-host"),
            wasmtime_cache_dir: Path::new("/tmp/wamn-dev-cache"),
        };
        let shared = Arc::new(Shared::default());
        let backend = FakeBackend::new(Arc::clone(&shared), [], [], []);

        let error = match activate_backend(request, pull_secret(), backend).await {
            Ok(_) => panic!("missing exact identity must refuse"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), DevActivationErrorKind::InvalidInput);
        assert_eq!(error.step(), "validate-identity");
        assert!(error.detail().contains("org"));
        assert!(
            shared
                .events
                .lock()
                .expect("event lock is not poisoned")
                .is_empty(),
            "validation must precede every side effect"
        );
    }
}
