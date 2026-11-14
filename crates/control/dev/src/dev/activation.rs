//! Native local-host activation for the development loop.
//!
//! Activation consumes caller-supplied infrastructure. It supervises one
//! `wamn-host` process and drives the upstream wash-runtime workload API; it
//! provisions nothing and owns no promotion verb.

pub(super) mod plan;
pub use plan::flow_http_identity_fields;
use plan::{
    decode_json, flow_http_request, heartbeat_matches, require_running, require_stopped,
    validate_request, workload_pending,
};

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;
use futures_util::StreamExt as _;
use rustix::process::{Pid, Signal, kill_process};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::process::{Child, Command};
use tokio::time::{Instant, timeout, timeout_at};
use wash_runtime::washlet::{OPERATOR_API_PREFIX, rpc_subject, types::v2};

use super::config::DevConfig;
use wamn_release_control::release_carrier::ReleaseCarrier;

/// Bound for connecting to the already-running development scheduler.
pub const SCHEDULER_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Bound for observing the exact local host's first heartbeat.
pub const HOST_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound for each native workload request and response.
pub const WORKLOAD_RPC_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound for flow-http to reach RUNNING after the host starts. The host compiles
/// the composed entry before it runs, which takes longer under load.
pub const FLOW_HTTP_START_TIMEOUT: Duration = Duration::from_secs(120);
/// Bound for graceful host shutdown before the kill-and-reap fallback.
///
/// The local release host has seven plugins and the chart drain delay: native cleanup
/// takes up to 51 seconds, then identity, probes, flush, and runtime exit add five.
/// Its cleared environment excludes ambient WASH timeout overrides.
pub const HOST_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(70);
/// Separate bound for reaping a host after the forced-kill fallback.
const HOST_REAP_TIMEOUT: Duration = Duration::from_secs(5);

const FLOW_HTTP_NAME: &str = "flow-http";
const FLOW_HTTP_WORKLOAD_ID: &str = "wamn-dev-flow-http";
const LOOPBACK_HTTP_BIND: &str = "127.0.0.1:0";
// The runtime-operator chart template sets the same HOME value.
const HOST_HOME: &str = "/tmp";

/// Exact deployment identity carried into the local serving process.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DevActivationIdentity {
    pub catalog: String,
    pub environment: String,
    pub org: String,
    pub project: String,
    pub host_group: String,
    pub host_name: String,
    pub runner: String,
}

impl DevActivationIdentity {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> String {
        wamn_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
}

/// Inputs produced by earlier stages and deployment-owned config.
pub struct DevActivationRequest<'a> {
    pub config: &'a DevConfig,
    pub applications: &'a wamn_catalog::EnvironmentApplications,
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
pub enum DevActivationErrorType {
    InvalidInput,
    SchedulerUnavailable,
    HostProcess,
    HeartbeatUnavailable,
    ProtocolViolation,
    WorkloadRefused,
    CleanupFailed,
}

impl DevActivationErrorType {
    /// Stable diagnostic code for this category.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "dev-activation-invalid-input",
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
    type_: DevActivationErrorType,
    step: &'static str,
    detail: Box<str>,
    source: Option<Box<dyn Error + Send + Sync>>,
}

impl DevActivationError {
    fn new(type_: DevActivationErrorType, step: &'static str, detail: impl Into<Box<str>>) -> Self {
        Self {
            type_,
            step,
            detail: detail.into(),
            source: None,
        }
    }

    fn with_source(
        type_: DevActivationErrorType,
        step: &'static str,
        detail: impl Into<Box<str>>,
        source: impl Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            type_,
            step,
            detail: detail.into(),
            source: Some(Box::new(source)),
        }
    }

    /// Stable error category.
    pub const fn error_type(&self) -> DevActivationErrorType {
        self.type_
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
            self.type_.as_str(),
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
                "project_env_keys",
                &self.env.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            )
            .finish()
    }
}

fn spawn_host_process(spec: &HostProcessSpec) -> anyhow::Result<Child> {
    if let Some((_, path)) = spec
        .env
        .iter()
        .find(|(name, _)| name == "WAMN_OAUTH_KEY_FILE")
    {
        wamn_environment::platform::oauth::local_key(Path::new(path))?;
    }
    let mut command = Command::new(&spec.program);
    command
        .args(spec.args.iter())
        .env_clear()
        .envs(spec.env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    command
        .spawn()
        .with_context(|| format!("spawn local wamn-host at {}", spec.program.display()))
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
    async fn connect(config: &DevConfig) -> Result<Self, DevActivationError> {
        let client = timeout(SCHEDULER_CONNECT_TIMEOUT, config.nats().connect())
            .await
            .map_err(|source| {
                DevActivationError::with_source(
                    DevActivationErrorType::SchedulerUnavailable,
                    "connect-scheduler",
                    "scheduler connection deadline expired",
                    source,
                )
            })?
            .map_err(|source| {
                DevActivationError::with_source(
                    DevActivationErrorType::SchedulerUnavailable,
                    "connect-scheduler",
                    "scheduler refused the NATS connection",
                    NativeBackendError(source),
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

#[expect(
    clippy::unused_async_trait_impl,
    reason = "`ActivationBackend` declares `async fn` because another implementation \
              talks to NATS; the verbs this backend answers without awaiting \
              still have to match the trait"
)]
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
        self.child = Some(spawn_host_process(spec)?);
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
        let request = async_nats::Request::new()
            .timeout(Some(deadline.saturating_duration_since(Instant::now())))
            .payload(payload.to_vec().into());
        let response = timeout_at(
            deadline,
            self.client.send_request(subject.to_owned(), request),
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
    http_port: u16,
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
            .field("http_port", &self.active.http_port)
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

    /// The workloads that the local host starts for this activation.
    pub fn workload_ids(&self) -> [&str; 1] {
        [self.workload_id()]
    }

    /// Loopback HTTP endpoint published by the supervised host heartbeat.
    pub fn http_base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.active.http_port)
    }

    /// Stop the workload, then terminate and reap the supervised host.
    pub async fn shutdown(self) -> Result<(), DevActivationError> {
        let BackendActivation {
            mut backend,
            host_id,
            http_port: _,
            workload_id,
        } = self.active;
        shutdown_backend(&mut backend, &host_id, &workload_id).await
    }
}

/// Validate and stage local workload bytes before a serving candidate is stopped.
pub(super) fn prepare_local(request: &DevActivationRequest<'_>) -> Result<(), DevActivationError> {
    validate_request(request)?;
    let local = request.config.local_artifacts();
    let stage_error = |source| {
        DevActivationError::with_source(
            DevActivationErrorType::InvalidInput,
            "local-flow-http",
            "cannot stage the explicit local flow-http component",
            source,
        )
    };
    std::fs::create_dir_all(&local.directory).map_err(stage_error)?;
    std::fs::set_permissions(&local.directory, std::fs::Permissions::from_mode(0o700))
        .map_err(stage_error)?;
    let manifest_bytes = std::fs::read(
        local
            .directory
            .join(wamn_catalog::RELEASE_MANIFEST_FILE_NAME),
    )
    .map_err(stage_error)?;
    let (manifest, digest) = wamn_catalog::ServingManifest::from_canonical_bytes(&manifest_bytes)
        .map_err(|source| stage_error(std::io::Error::other(source)))?;
    if digest != request.release.manifest_digest {
        return Err(stage_error(std::io::Error::other(
            "local release digest mismatch",
        )));
    }
    let entry = manifest.composed_entry.as_ref().ok_or_else(|| {
        stage_error(std::io::Error::other(
            "publish the release with its composed HTTP entry",
        ))
    })?;
    let entry_path = entry.reference.strip_prefix("file:").ok_or_else(|| {
        stage_error(std::io::Error::other(
            "local entry requires a file reference",
        ))
    })?;
    let component_bytes = std::fs::read(entry_path).map_err(stage_error)?;
    let result = (|| -> anyhow::Result<()> {
        let engine = wamn_engine::engine::build_engine(&[])?;
        let component =
            wash_runtime::wasmtime::component::Component::new(engine.inner(), &component_bytes)?;
        anyhow::ensure!(
            component
                .component_type()
                .exports(engine.inner())
                .any(|(name, _)| name.starts_with("wasi:http/incoming-handler@")
                    || name == "wasi:http/handler@0.3.0"),
            "local flow-http component has no incoming HTTP handler"
        );
        Ok(())
    })();
    result.map_err(|source| {
        DevActivationError::with_source(
            DevActivationErrorType::InvalidInput,
            "local-flow-http",
            format!("local flow-http bytes failed native component validation: {source:#}"),
            NativeBackendError::from(source),
        )
    })?;
    let mut workload = flow_http_request(request);
    let component = &mut workload
        .workload
        .as_mut()
        .expect("flow-http workload exists")
        .wit_world
        .as_mut()
        .expect("flow-http world exists")
        .components[0];
    component
        .local_resources
        .get_or_insert_with(Default::default)
        .config
        .insert(
            wamn_catalog::http_routes::HTTP_ROUTES_CONFIG.into(),
            serde_json::to_string(&wamn_catalog::http_routes::route_data(
                &manifest,
                manifest.digest().as_str(),
            ))
            .expect("route data is JSON"),
        );
    component.image.clone_from(&entry.reference);
    component.image_pull_policy = v2::ImagePullPolicy::Never.into();
    let bytes = serde_json::to_vec(&workload).map_err(|source| {
        DevActivationError::with_source(
            DevActivationErrorType::InvalidInput,
            "local-flow-http",
            "cannot encode the local workload",
            source,
        )
    })?;
    std::fs::write(local.directory.join("flow-http.json"), bytes).map_err(|source| {
        DevActivationError::with_source(
            DevActivationErrorType::InvalidInput,
            "local-flow-http",
            "cannot stage the local workload",
            source,
        )
    })?;
    Ok(())
}

/// Start one local host and its flow-http workload through native NATS.
pub async fn activate(
    request: DevActivationRequest<'_>,
) -> Result<DevActivation, DevActivationError> {
    validate_request(&request)?;
    staged_http_reference(&request)?;
    let backend = NativeActivationBackend::connect(request.config).await?;
    let active = activate_backend(request, backend).await?;
    Ok(DevActivation { active })
}

fn staged_http_reference(request: &DevActivationRequest<'_>) -> Result<String, DevActivationError> {
    let path = request
        .config
        .local_artifacts()
        .directory
        .join("flow-http.json");
    let bytes = std::fs::read(&path).map_err(|source| {
        DevActivationError::with_source(
            DevActivationErrorType::InvalidInput,
            "read-local-workload",
            "read the staged HTTP workload",
            source,
        )
    })?;
    let staged: v2::WorkloadStartRequest = serde_json::from_slice(&bytes).map_err(|source| {
        DevActivationError::with_source(
            DevActivationErrorType::InvalidInput,
            "read-local-workload",
            "decode the staged HTTP workload",
            source,
        )
    })?;
    staged
        .workload
        .as_ref()
        .and_then(|workload| workload.wit_world.as_ref())
        .and_then(|world| world.components.first())
        .map(|component| component.image.clone())
        .ok_or_else(|| {
            DevActivationError::new(
                DevActivationErrorType::InvalidInput,
                "read-local-workload",
                "the staged HTTP workload has no component",
            )
        })
}

fn host_process_spec(request: &DevActivationRequest<'_>) -> HostProcessSpec {
    plan::host_process_spec(request)
}

async fn activate_backend<B>(
    request: DevActivationRequest<'_>,
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
            DevActivationErrorType::HostProcess,
            "spawn-host",
            "local wamn-host could not start",
            source,
        )
    })?;

    let mut host_id = None;
    let mut http_port = None;
    let activation = async {
        let selected = wait_for_host(&mut backend, request.identity).await?;
        host_id = Some(selected.id);
        http_port = Some(selected.http_port);
        let selected = host_id
            .as_deref()
            .expect("selected host identity is assigned before workload start");
        let deadline = Instant::now() + FLOW_HTTP_START_TIMEOUT;
        loop {
            let response: v2::WorkloadStatusResponse = request_json(
                &mut backend,
                &rpc_subject(selected, "workload.status"),
                &v2::WorkloadStatusRequest {
                    workload_id: FLOW_HTTP_WORKLOAD_ID.to_owned(),
                },
                "status-local-workload",
            )
            .await?;
            if response
                .workload_status
                .as_ref()
                .is_some_and(|status| status.workload_state == v2::WorkloadState::Running as i32)
            {
                break;
            }
            if !workload_pending(response.workload_status.as_ref()) || Instant::now() >= deadline {
                require_running(
                    "status-local-workload",
                    response.workload_status,
                    FLOW_HTTP_WORKLOAD_ID,
                )?;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

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
        http_port: http_port.expect("successful activation selected one HTTP port"),
        workload_id: FLOW_HTTP_WORKLOAD_ID.into(),
    })
}

#[derive(Debug)]
struct SelectedHost {
    id: String,
    http_port: u16,
}

async fn wait_for_host<B>(
    backend: &mut B,
    identity: &DevActivationIdentity,
) -> Result<SelectedHost, DevActivationError>
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
                    DevActivationErrorType::HeartbeatUnavailable,
                    "wait-heartbeat",
                    "the exact local host did not publish before the deadline",
                    source,
                )
            })?
            .ok_or_else(|| {
                DevActivationError::new(
                    DevActivationErrorType::HeartbeatUnavailable,
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
            let http_port = u16::try_from(heartbeat.http_port)
                .ok()
                .filter(|port| *port != 0)
                .ok_or_else(|| {
                    DevActivationError::new(
                        DevActivationErrorType::ProtocolViolation,
                        "wait-heartbeat",
                        format!(
                            "the supervised HTTP host reported invalid port {}",
                            heartbeat.http_port
                        ),
                    )
                })?;
            return Ok(SelectedHost {
                id: heartbeat.id,
                http_port,
            });
        }
    }
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
                DevActivationErrorType::CleanupFailed,
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
                    DevActivationErrorType::CleanupFailed,
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
                    DevActivationErrorType::CleanupFailed,
                    "kill-host",
                    "kill the unresponsive local host",
                    source,
                )
            });
        }
        if let Err(source) = backend.reap_host(Instant::now() + HOST_REAP_TIMEOUT).await {
            first_error.get_or_insert_with(|| {
                DevActivationError::with_source(
                    DevActivationErrorType::CleanupFailed,
                    "reap-host",
                    "reap the killed local host before the deadline",
                    source,
                )
            });
        }
    }

    first_error.map_or(Ok(()), Err)
}

fn backend_error(
    step: &'static str,
    source: impl Error + Send + Sync + 'static,
) -> DevActivationError {
    DevActivationError::with_source(
        DevActivationErrorType::SchedulerUnavailable,
        step,
        "native runtime NATS operation failed",
        source,
    )
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
            DevActivationErrorType::ProtocolViolation,
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use serde_json::json;
    use wamn_catalog::ManifestDigest;

    use super::*;
    use crate::dev::config::parse_config;

    fn test_applications() -> wamn_catalog::EnvironmentApplications {
        serde_json::from_value(serde_json::json!({"receiving": {"packages": ["base", "overlay"]}}))
            .unwrap()
    }

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
        reap_deadline: Mutex<Option<Instant>>,
    }

    struct FakeBackend {
        shared: Arc<Shared>,
        heartbeats: VecDeque<RuntimeMessage>,
        responses: VecDeque<Box<[u8]>>,
        waits: VecDeque<Option<bool>>,
        host_exit_after: Duration,
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
                host_exit_after: Duration::ZERO,
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

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "`ActivationBackend` declares `async fn` because the native backend talks \
                  to NATS; this fake answers from memory and still has to match \
                  the trait"
    )]
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

        async fn wait_host(&mut self, deadline: Instant) -> Result<Option<bool>, Self::Error> {
            self.record(Event::Wait);
            if Instant::now() + self.host_exit_after > deadline {
                return Ok(None);
            }
            self.waits
                .pop_front()
                .ok_or(FakeError("no wait outcome configured"))
        }

        async fn kill_host(&mut self) -> Result<(), Self::Error> {
            self.record(Event::Kill);
            Ok(())
        }

        async fn reap_host(&mut self, deadline: Instant) -> Result<(), Self::Error> {
            self.record(Event::Reap);
            *self
                .shared
                .reap_deadline
                .lock()
                .expect("reap-deadline lock is not poisoned") = Some(deadline);
            Ok(())
        }
    }

    fn config() -> DevConfig {
        parse_config(&serde_json::to_vec(&config_document()).unwrap())
            .expect("complete activation config parses")
    }

    fn config_document() -> serde_json::Value {
        json!({
            "target_database_url": "postgresql://target:target-secret@127.0.0.1:41002/target",
            "migration_database_url": "postgresql://migration:migration-secret@127.0.0.1:41002/target",
            "system_database_url": "postgresql://owner:owner-secret@127.0.0.1:41003/system",
            "identity_database_url": "postgresql://identity:identity-secret@127.0.0.1:41004/system",
            "guest_database_url": "postgresql://guest:guest-secret@127.0.0.1:41005/target",
            "executor_platform_database_url": "postgresql://platform:platform-secret@127.0.0.1:41006/target",
            "http_admitter_database_url": "postgresql://admitter:admitter-secret@127.0.0.1:41007/target",
            "administration_database_url": "postgresql://administration:administration-secret@127.0.0.1:41013/target",
            "scheduler_nats_url": "nats://127.0.0.1:41009",
            "nats_tls_ca": "/run/wamn-dev/nats/ca.crt",
            "nats_tls_cert": "/run/wamn-dev/nats/client.crt",
            "nats_tls_key": "/run/wamn-dev/nats/client.key",
            "stream_replicas": 1,
            "dup_window_secs": 120,
            "otel_exporter_otlp_endpoint": "http://127.0.0.1:41016",
            "condition_bearer_token": "condition-secret",
            "route_host": "fixture.localhost",
            "platform_domain": "example.invalid",
            "local_artifacts": {
                "directory": "/tmp/wamn-local-candidate", "flow_http_component": "/tmp/flow-http.wasm"
            },
            "package_sources": [],
            "catalog": "default",
            "environment": "fixture-dev",
            "org": "example",
            "project": "fixture",
            "host_group": "wamn-dev-fixture",
            "host_name": "wamn-dev-fixture-1",
            "runner": "wamn-dev-fixture-1",
            "host_binary": "/opt/wamn/bin/wamn-host",
            "wasmtime_cache_dir": "/tmp/wamn-dev-cache",
        })
    }

    fn identity() -> DevActivationIdentity {
        DevActivationIdentity {
            catalog: "default".to_owned(),
            environment: "fixture-dev".to_owned(),
            org: "example".to_owned(),
            project: "fixture".to_owned(),
            host_group: "wamn-dev-fixture".to_owned(),
            host_name: "wamn-dev-fixture-1".to_owned(),
            runner: "wamn-dev-fixture-1".to_owned(),
        }
    }

    /// The deploy template and the production path must declare the SAME
    /// identity fields. Either side gaining or losing one fails here.
    ///
    /// Read from the template rather than restated, and compared against the
    /// producer rather than a second list, so this cannot drift into the third
    /// hand-maintained copy of the thing it exists to prevent.
    fn release() -> ReleaseCarrier {
        ReleaseCarrier {
            artifact_base: "127.0.0.1:41012/wamn/releases".to_owned(),
            manifest_digest: ManifestDigest::parse(format!("sha256:{}", "7".repeat(64)))
                .expect("fixture digest is canonical"),
        }
    }

    fn heartbeat(id: &str, hostname: &str, environment: &str, host_group: &str) -> RuntimeMessage {
        heartbeat_with_port(id, hostname, environment, host_group, 38_080)
    }

    fn heartbeat_with_port(
        id: &str,
        hostname: &str,
        environment: &str,
        host_group: &str,
        http_port: u32,
    ) -> RuntimeMessage {
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
            http_port,
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

    #[test]
    fn session_identity_reaches_the_host_without_ambient_environment() {
        let mut document = config_document();
        document["session_identity"] = serde_json::json!({
            "issuer": "https://127.0.0.1:8443", "ca": "/tmp/owned-identity-ca.pem", "instance_suffix": "abc12345"
        });
        let config = parse_config(&serde_json::to_vec(&document).unwrap()).unwrap();
        let release = release();
        let identity = identity();
        let request = DevActivationRequest {
            applications: &test_applications(),
            config: &config,
            release: &release,
            identity: &identity,
            host_binary: config.host_binary(),
            wasmtime_cache_dir: config.wasmtime_cache_dir(),
        };
        let spec = host_process_spec(&request);
        for (name, value) in [
            ("WAMN_SESSION_ISSUER", "https://127.0.0.1:8443"),
            ("WAMN_SESSION_JWKS_CA", "/tmp/owned-identity-ca.pem"),
            ("WAMN_SESSION_INSTANCE_SUFFIX", "abc12345"),
        ] {
            assert!(
                spec.env
                    .iter()
                    .any(|(key, actual)| key == name && actual == value)
            );
        }
        document["session_identity"]["issuer"] = "http://127.0.0.1:8443".into();
        assert!(parse_config(&serde_json::to_vec(&document).unwrap()).is_err());
    }

    /// The host reads credentials through the reader that `dev.json` names.
    #[test]
    fn the_credential_reader_reaches_the_host() {
        let mut document = config_document();
        let config = parse_config(&serde_json::to_vec(&document).unwrap()).unwrap();
        let release = release();
        let identity = identity();
        let request = DevActivationRequest {
            applications: &test_applications(),
            config: &config,
            release: &release,
            identity: &identity,
            host_binary: config.host_binary(),
            wasmtime_cache_dir: config.wasmtime_cache_dir(),
        };
        let spec = host_process_spec(&request);
        assert!(
            !spec
                .env
                .iter()
                .any(|(key, _)| key == "WAMN_CREDENTIALS_FILE")
        );
        document["connection_database_url"] = "postgres://reader:password@localhost/system".into();
        let config = parse_config(&serde_json::to_vec(&document).unwrap()).unwrap();
        let request = DevActivationRequest {
            config: &config,
            ..request
        };
        let spec = host_process_spec(&request);
        assert!(
            spec.env
                .iter()
                .any(|(key, value)| key == "WAMN_CONNECTION_STORE_URL"
                    && value == "postgres://reader:password@localhost/system")
        );
        for path in ["/tmp/owned-root/credentials.json", "credentials.json"] {
            document["credentials_file"] = path.into();
            assert!(parse_config(&serde_json::to_vec(&document).unwrap()).is_err());
        }
    }

    #[tokio::test]
    async fn activation_uses_exact_native_sequence_and_bounded_cleanup() {
        let config = config();
        let identity = identity();
        let release = release();
        let request = DevActivationRequest {
            applications: &test_applications(),
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
                response(&v2::WorkloadStatusResponse {
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
        let workload = flow_http_request(&request);

        let mut active = activate_backend(request, backend)
            .await
            .expect("the exact local host activates");
        assert_eq!(&*active.host_id, "selected-host-id");
        assert_eq!(active.http_port, 38_080);
        let shutdown_started = Instant::now();
        shutdown_backend(&mut active.backend, &active.host_id, &active.workload_id)
            .await
            .expect("timeout falls back to kill and bounded reap");
        let shutdown_finished = Instant::now();
        let reap_deadline = shared
            .reap_deadline
            .lock()
            .expect("reap-deadline lock is not poisoned")
            .expect("forced kill has a reap deadline");
        assert!(reap_deadline >= shutdown_started + Duration::from_secs(5));
        assert!(reap_deadline <= shutdown_finished + Duration::from_secs(5));

        let spec = shared
            .spec
            .lock()
            .expect("process-spec lock is not poisoned")
            .clone()
            .expect("one host was spawned");
        assert_eq!(spec.program, Path::new("/opt/wamn/bin/wamn-host"));
        let incoming_http = workload
            .workload
            .as_ref()
            .and_then(|workload| workload.wit_world.as_ref())
            .and_then(|world| {
                world.host_interfaces.iter().find(|interface| {
                    interface.namespace == "wasi"
                        && interface.package == "http"
                        && interface.version == "0.3.0"
                        && interface.interfaces == ["handler"]
                })
            })
            .expect("flow-http declares the native incoming HTTP interface");
        assert_eq!(
            incoming_http.config,
            HashMap::from([("host".to_owned(), "fixture.localhost".to_owned())])
        );
    }

    #[tokio::test]
    async fn shutdown_allows_native_cleanup_then_accepts_sigterm_exit_without_kill_fallback() {
        let shared = Arc::new(Shared::default());
        let mut backend = FakeBackend::new(
            Arc::clone(&shared),
            [],
            [response(&v2::WorkloadStopResponse {
                workload_status: Some(status(v2::WorkloadState::Stopping)),
            })],
            [Some(false)],
        );
        // Model the full default local-host exit envelope without a wall-clock wait.
        backend.host_exit_after = Duration::from_secs(56);

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
    async fn a_heartbeat_without_a_usable_http_port_refuses_the_activation() {
        // The port reaches the read seam as the loopback route a developer is
        // told to call. Accepting zero would publish http://127.0.0.1:0.
        let identity = identity();
        let mut backend = FakeBackend::new(
            Arc::new(Shared::default()),
            [heartbeat_with_port(
                "selected-host-id",
                &identity.host_name,
                &identity.environment,
                &identity.host_group,
                0,
            )],
            [],
            [],
        );

        let error = wait_for_host(&mut backend, &identity)
            .await
            .expect_err("a zero HTTP port is not a usable endpoint");

        assert_eq!(
            error.error_type(),
            DevActivationErrorType::ProtocolViolation
        );
        assert!(
            error.to_string().contains("invalid port 0"),
            "{error} must name the refused port"
        );
    }

    #[tokio::test]
    async fn a_failed_flow_http_start_refuses_before_the_deadline() {
        // NOT_FOUND and STARTING may still reach RUNNING, so the poll waits;
        // ERROR is final, so the poll refuses at once.
        let config = config();
        let identity = identity();
        let release = release();
        let request = DevActivationRequest {
            applications: &test_applications(),
            config: &config,
            release: &release,
            identity: &identity,
            host_binary: Path::new("/opt/wamn/bin/wamn-host"),
            wasmtime_cache_dir: Path::new("/tmp/wamn-dev-cache"),
        };
        let backend = FakeBackend::new(
            Arc::new(Shared::default()),
            [heartbeat(
                "selected-host-id",
                &identity.host_name,
                &identity.environment,
                &identity.host_group,
            )],
            [
                response(&v2::WorkloadStatusResponse {
                    workload_status: Some(status(v2::WorkloadState::NotFound)),
                }),
                response(&v2::WorkloadStatusResponse {
                    workload_status: Some(status(v2::WorkloadState::Starting)),
                }),
                response(&v2::WorkloadStatusResponse {
                    workload_status: Some(status(v2::WorkloadState::Error)),
                }),
                response(&v2::WorkloadStopResponse {
                    workload_status: Some(status(v2::WorkloadState::Stopping)),
                }),
            ],
            [None],
        );

        let started = Instant::now();
        let Err(error) = activate_backend(request, backend).await else {
            panic!("a workload in ERROR must refuse")
        };

        assert_eq!(error.error_type(), DevActivationErrorType::WorkloadRefused);
        assert_eq!(error.step(), "status-local-workload");
        assert!(error.detail().contains("WORKLOAD_STATE_ERROR"), "{error}");
        assert!(started.elapsed() < FLOW_HTTP_START_TIMEOUT / 4);
    }

    #[tokio::test]
    async fn invalid_identity_refuses_before_subscription_or_process_spawn() {
        let config = config();
        let mut identity = identity();
        identity.org.clear();
        let release = release();
        let request = DevActivationRequest {
            applications: &test_applications(),
            config: &config,
            release: &release,
            identity: &identity,
            host_binary: Path::new("/opt/wamn/bin/wamn-host"),
            wasmtime_cache_dir: Path::new("/tmp/wamn-dev-cache"),
        };
        let shared = Arc::new(Shared::default());
        let backend = FakeBackend::new(Arc::clone(&shared), [], [], []);

        let Err(error) = activate_backend(request, backend).await else {
            panic!("missing exact identity must refuse")
        };

        assert_eq!(error.error_type(), DevActivationErrorType::InvalidInput);
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
