//! Native local-host activation for the development loop.
//!
//! Activation consumes caller-supplied infrastructure. It supervises one
//! `wamn-host` process and drives the upstream wash-runtime workload API; it
//! provisions nothing and owns no promotion verb.

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
use crate::release_carrier::ReleaseCarrier;

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
const MATERIALIZER_WORKLOAD_ID: &str = "wamn-dev-materializer";
/// The platform materializer's one template, which the cluster files and the
/// cluster tests render too.
const MATERIALIZER_TEMPLATE: &str =
    include_str!("../../../../../deploy/platform/materializer.example.yaml");
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
    pub schema: String,
    pub host_group: String,
    pub host_name: String,
    pub runner: String,
}

impl DevActivationIdentity {
    /// The composed environment key derived from the environment coordinate.
    pub fn env_key(&self) -> String {
        wamn_control_registry::project_env_key(&self.org, &self.project, &self.environment)
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
    fn new(kind: DevActivationErrorType, step: &'static str, detail: impl Into<Box<str>>) -> Self {
        Self {
            type_: kind,
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
                "env_keys",
                &self.env.iter().map(|(key, _)| key).collect::<Vec<_>>(),
            )
            .finish()
    }
}

fn spawn_host_process(spec: &HostProcessSpec) -> anyhow::Result<Child> {
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
    async fn connect(url: &str) -> Result<Self, DevActivationError> {
        let client = timeout(SCHEDULER_CONNECT_TIMEOUT, async_nats::connect(url))
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
    pub fn workload_ids(&self) -> [&str; 2] {
        [self.workload_id(), MATERIALIZER_WORKLOAD_ID]
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
    let checked = (|| -> anyhow::Result<()> {
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
    checked.map_err(|source| {
        DevActivationError::with_source(
            DevActivationErrorType::InvalidInput,
            "local-flow-http",
            format!("local flow-http bytes failed native component validation: {source:#}"),
            NativeBackendError::from(source),
        )
    })?;
    let mut workload = flow_http_request(request);
    let world = workload
        .workload
        .as_mut()
        .expect("HTTP workload exists")
        .wit_world
        .as_mut()
        .expect("HTTP world exists");
    for name in wamn_engine::statement_instances::statement_instances(&component_bytes)
        .map_err(|source| stage_error(std::io::Error::other(source)))?
        .keys()
    {
        let mut interface = wit_interface("wamn", "postgres", "0.4.0", "statements");
        interface.name.clone_from(name);
        world.host_interfaces.push(interface);
    }
    let component = &mut workload
        .workload
        .as_mut()
        .expect("flow-http workload exists")
        .wit_world
        .as_mut()
        .expect("flow-http world exists")
        .components[0];
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
    stage_materializer(request.config)
}

/// Stage the platform materializer beside the release. It enters neither the
/// local admission facts nor the release manifest.
fn stage_materializer(config: &DevConfig) -> Result<(), DevActivationError> {
    let staged = (|| -> anyhow::Result<()> {
        let local = config.local_artifacts();
        let bytes = std::fs::read(&local.materializer_component).with_context(|| {
            format!(
                "read the materializer component {}",
                local.materializer_component.display()
            )
        })?;
        let digest = wamn_engine::component_admission::component_digest(&bytes);
        let path = wamn_engine::artifact_source::local_component_path(&local.directory, &digest)
            .expect("a computed component digest is valid");
        std::fs::write(path, bytes)?;
        let request = materializer_request(config, &digest)?;
        std::fs::write(
            local.directory.join("materializer.json"),
            serde_json::to_vec(&request)?,
        )?;
        Ok(())
    })();
    staged.map_err(|source| {
        DevActivationError::with_source(
            DevActivationErrorType::InvalidInput,
            "local-materializer",
            format!("cannot stage the local materializer: {source:#}"),
            NativeBackendError::from(source),
        )
    })
}

/// The cluster's materializer workload for this environment, rendered from its
/// template with the cluster's settings, and with the local bytes of `digest`.
fn materializer_request(
    config: &DevConfig,
    digest: &str,
) -> anyhow::Result<v2::WorkloadStartRequest> {
    use wamn_control_provision::materializer_workload::{
        EventIdentity, FETCH_MS, MaterializerInput, SWEEP_MS, render_materializer,
    };

    let identity = config.activation_identity();
    let source = wamn_control_provision::events::source_stream_config(
        &wamn_control_registry::Triple::new(
            &identity.org,
            &identity.project,
            identity.environment.clone(),
        ),
        config.stream_replicas(),
        Duration::from_secs(config.dup_window_secs()),
    );
    let rendered = render_materializer(
        MATERIALIZER_TEMPLATE,
        &MaterializerInput {
            workload: format!("{}-materializer", identity.project),
            namespace: host_namespace(identity).to_owned(),
            image: digest.to_owned(),
            env_key: identity.env_key(),
            event: EventIdentity {
                org: identity.org.clone(),
                project: identity.project.clone(),
                environment: identity.environment.clone(),
            },
            event_stream: source.name,
            fetch_ms: FETCH_MS,
            sweep_ms: SWEEP_MS,
        },
    )
    .context("render the materializer workload")?;
    let document: rendered::Document =
        serde_yaml::from_str(&rendered).context("read the rendered materializer workload")?;
    let workload = document.spec.template.spec;
    let resources = workload.service.local_resources;
    Ok(v2::WorkloadStartRequest {
        workload_id: MATERIALIZER_WORKLOAD_ID.to_owned(),
        workload: Some(v2::Workload {
            namespace: host_namespace(identity).to_owned(),
            name: document.metadata.name,
            annotations: HashMap::new(),
            service: Some(v2::Service {
                image: digest.to_owned(),
                local_resources: Some(v2::LocalResources {
                    memory_limit_mb: 0,
                    cpu_limit: 0,
                    config: resources.config,
                    environment: resources.environment.config,
                    volume_mounts: Vec::new(),
                    allowed_hosts: Vec::new(),
                    allowed_ip_name_lookups: resources.allowed_ip_name_lookups,
                    allowed_host_loopback_ports: Vec::new(),
                }),
                max_restarts: workload.service.max_restarts,
                image_pull_secret: None,
                image_pull_policy: v2::ImagePullPolicy::Never.into(),
            }),
            wit_world: Some(v2::WitWorld {
                components: Vec::new(),
                host_interfaces: workload
                    .host_interfaces
                    .into_iter()
                    .map(|interface| v2::WitInterface {
                        namespace: interface.namespace,
                        package: interface.package,
                        version: interface.version,
                        interfaces: interface.interfaces,
                        config: interface.config,
                        name: interface.name.unwrap_or_default(),
                    })
                    .collect(),
            }),
            volumes: Vec::new(),
        }),
    })
}

/// The fields of a rendered materializer workload that the loop starts. A
/// field the template gains that the loop does not map refuses here, so the
/// loop cannot run less than the cluster runs.
mod rendered {
    use std::collections::HashMap;

    use serde::Deserialize;

    #[derive(Deserialize)]
    pub(super) struct Document {
        pub(super) metadata: Metadata,
        pub(super) spec: Spec,
    }

    #[derive(Deserialize)]
    pub(super) struct Metadata {
        pub(super) name: String,
    }

    #[derive(Deserialize)]
    pub(super) struct Spec {
        pub(super) template: Template,
    }

    #[derive(Deserialize)]
    pub(super) struct Template {
        pub(super) spec: Workload,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub(super) struct Workload {
        pub(super) host_interfaces: Vec<Interface>,
        pub(super) service: Service,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct Interface {
        pub(super) namespace: String,
        pub(super) package: String,
        pub(super) version: String,
        pub(super) interfaces: Vec<String>,
        #[serde(default)]
        pub(super) name: Option<String>,
        #[serde(default)]
        pub(super) config: HashMap<String, String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    pub(super) struct Service {
        #[expect(dead_code, reason = "the loop starts local bytes, not the image")]
        pub(super) image: String,
        #[expect(dead_code, reason = "the loop starts local bytes and pulls nothing")]
        pub(super) image_pull_policy: String,
        pub(super) max_restarts: u64,
        pub(super) local_resources: Resources,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    pub(super) struct Resources {
        #[serde(default)]
        pub(super) allowed_ip_name_lookups: Vec<String>,
        pub(super) config: HashMap<String, String>,
        pub(super) environment: Environment,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(super) struct Environment {
        pub(super) config: HashMap<String, String>,
    }
}

/// Start one local host and its flow-http workload through native NATS.
pub async fn activate(
    request: DevActivationRequest<'_>,
) -> Result<DevActivation, DevActivationError> {
    validate_request(&request)?;
    staged_http_reference(&request)?;
    let backend = NativeActivationBackend::connect(request.config.scheduler_nats_url()).await?;
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

fn validate_request(request: &DevActivationRequest<'_>) -> Result<(), DevActivationError> {
    let identity = request.identity;
    for (field, value) in [
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
                DevActivationErrorType::InvalidInput,
                "validate-identity",
                format!("supply the required activation identity {field}"),
            ));
        }
    }
    if request.release.artifact_base.is_empty() {
        return Err(DevActivationError::new(
            DevActivationErrorType::InvalidInput,
            "validate-release",
            "supply the release artifact base",
        ));
    }
    if request.host_binary.as_os_str().is_empty() {
        return Err(DevActivationError::new(
            DevActivationErrorType::InvalidInput,
            "validate-host",
            "supply the wamn-host executable path",
        ));
    }
    if request.wasmtime_cache_dir.as_os_str().is_empty() {
        return Err(DevActivationError::new(
            DevActivationErrorType::InvalidInput,
            "validate-host",
            "supply the Wasmtime cache directory",
        ));
    }
    Ok(())
}

/// The host namespace of the local host (docs/plan/platform-deploy.md §16):
/// what it reads from `WASMCLOUD_HOST_ENVIRONMENT`, names in its heartbeat,
/// and where its workloads are placed. A local session has no Kubernetes
/// namespace of its own, so its workloads take the environment name.
fn host_namespace(identity: &DevActivationIdentity) -> &str {
    &identity.environment
}

fn host_process_spec(request: &DevActivationRequest<'_>) -> HostProcessSpec {
    let platform: serde_yaml::Value = serde_yaml::from_str(include_str!(
        "../../../../../deploy/platform/release/values.yaml"
    ))
    .expect("the platform chart defaults are valid YAML");
    let runtime = &platform["runtime-operator"]["runtime"];
    let group: serde_yaml::Value = serde_yaml::from_str(include_str!(
        "../../../../../deploy/gcp/host-group-receiving.yaml"
    ))
    .expect("the Receiving host group is valid YAML");
    let identity = request.identity;
    let event_identity = request.config.activation_identity();
    let local = request.config.local_artifacts();
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
        "--applications".to_owned(),
        serde_json::to_string(request.applications).expect("application map serializes"),
        "--http-addr".to_owned(),
        LOOPBACK_HTTP_BIND.to_owned(),
        "--local-application".to_owned(),
        local.directory.display().to_string(),
        "--local-application-digest".to_owned(),
        request.release.manifest_digest.to_string(),
        "--wasmtime-cache-dir".to_owned(),
        request.wasmtime_cache_dir.display().to_string(),
        "--materializer-nats-binding-file".to_owned(),
        request
            .config
            .materializer_nats_binding_file()
            .display()
            .to_string(),
        "--project".to_owned(),
        identity.project.clone(),
        "--org".to_owned(),
        identity.org.clone(),
        "--schema".to_owned(),
        identity.schema.clone(),
        "--drain-bound-seconds".to_owned(),
        HOST_SHUTDOWN_TIMEOUT.as_secs().to_string(),
    ];
    for (field, flag) in [
        ("drainDelaySeconds", "--drain-delay"),
        ("natsConnectTimeoutSeconds", "--nats-connect-timeout"),
    ] {
        args.extend([
            flag.to_owned(),
            format!("{}s", runtime[field].as_u64().expect("chart timeout")),
        ]);
    }
    args.extend([
        "--socket-egress".to_owned(),
        crate::release_chart::SOCKET_EGRESS.to_owned(),
    ]);
    if runtime["probes"]["endpoint"]["enabled"].as_bool() == Some(true) {
        args.extend(["--probe-addr".to_owned(), LOOPBACK_HTTP_BIND.to_owned()]);
    }
    let mut env = vec![
        (
            "WASMCLOUD_HOST_ENVIRONMENT".to_owned(),
            host_namespace(identity).to_owned(),
        ),
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
            "WAMN_ADMINISTRATION_PG_URL".to_owned(),
            request.config.administration_database_url().to_owned(),
        ),
        (
            "WAMN_EVT_NATS_URL".to_owned(),
            request.config.event_nats_url().to_owned(),
        ),
        (
            "WAMN_EVT_NATS_USERNAME".to_owned(),
            request.config.event_nats_username().to_owned(),
        ),
        (
            "WAMN_EVT_NATS_PASSWORD_FILE".to_owned(),
            request
                .config
                .event_nats_password_file()
                .display()
                .to_string(),
        ),
        (
            "WAMN_EVT_STREAM_REPLICAS".to_owned(),
            request.config.stream_replicas().to_string(),
        ),
        (
            "WAMN_EVT_DUP_WINDOW_SECS".to_owned(),
            request.config.dup_window_secs().to_string(),
        ),
        ("WAMN_EVT_ORG".to_owned(), event_identity.org.clone()),
        (
            "WAMN_EVT_PROJECT".to_owned(),
            event_identity.project.clone(),
        ),
        (
            "WAMN_EVT_ENV".to_owned(),
            event_identity.environment.clone(),
        ),
    ];
    env.push((
        "WAMN_MATERIALIZER_DRAIN_BOUND_SECONDS".to_owned(),
        crate::release_chart::MATERIALIZER_DRAIN_BOUND_SECONDS.to_string(),
    ));
    env.extend(
        crate::release_chart::TELEMETRY_VARIABLES.map(|(name, value)| {
            let value = if name == "OTEL_EXPORTER_OTLP_ENDPOINT" {
                request.config.otel_exporter_otlp_endpoint()
            } else {
                value
            };
            (name.to_owned(), value.to_owned())
        }),
    );
    env.push(("HOME".to_owned(), HOST_HOME.to_owned()));
    for variable in runtime["env"]
        .as_sequence()
        .expect("platform host environment")
    {
        env.push((
            variable["name"]
                .as_str()
                .expect("environment name")
                .to_owned(),
            variable["value"]
                .as_str()
                .expect("environment value")
                .to_owned(),
        ));
    }
    for variable in group["env"].as_sequence().expect("host group environment") {
        let name = variable["name"].as_str().expect("environment name");
        if [
            "WAMN_COMPONENT_POOL_SIZE",
            "WAMN_PG_GUEST_POOL_MAX",
            "WAMN_PG_PLATFORM_POOL_MAX",
            "WAMN_HTTP_ROUTE_IN_FLIGHT_LIMIT",
        ]
        .contains(&name)
        {
            env.push((
                name.to_owned(),
                variable["value"].as_str().expect("tuning value").to_owned(),
            ));
        }
    }
    let resources = &group["resources"];
    for (name, value) in [
        ("WASH_DEFAULT_HEAP_MEMORY", &resources["defaultHeapMemory"]),
        ("WASH_CORE_INSTANCES", &resources["coreInstances"]),
        ("WASH_GUEST_MEMORY_MODE", &resources["guestMemoryMode"]),
        (
            "WASH_MAX_CONCURRENT_STARTS",
            &resources["maxConcurrentStarts"],
        ),
    ] {
        env.push((
            name.to_owned(),
            value.as_str().map_or_else(
                || value.as_u64().expect("host resource value").to_string(),
                str::to_owned,
            ),
        ));
    }
    if let Some(url) = request.config.connection_database_url() {
        env.push(("WAMN_CONNECTION_STORE_URL".to_owned(), url.to_owned()));
    }
    if let Some(identity) = request.config.session_identity() {
        env.extend([
            ("WAMN_SESSION_ISSUER".to_owned(), identity.issuer.clone()),
            (
                "WAMN_SESSION_JWKS_CA".to_owned(),
                identity.ca.display().to_string(),
            ),
            (
                "WAMN_SESSION_INSTANCE_SUFFIX".to_owned(),
                identity.instance_suffix.clone(),
            ),
        ]);
    }
    HostProcessSpec {
        program: request.host_binary.to_owned(),
        args: args.into_boxed_slice(),
        env: env.into_boxed_slice(),
    }
}

/// The identity claims the host injects into the flow-http workload, paired
/// with the field of [`DevActivationIdentity`] each one carries.
///
/// This is the SINGLE SOURCE for the claim set. It exists because the set was
/// previously written out by hand in two places -- here and in the cluster
/// journey's workload renderer -- and the second copy had four of the five.
/// `wamn.catalog` was left at the template's placeholder, with no identity check, no
/// substitution and no assertion, so a second application would have deployed
/// under the first one's catalog and route resolution would have looked in
/// the wrong one. An enumeration maintained by hand on both sides misses
/// whatever the enumeration missed; nothing compared the two.
///
/// `deploy/platform/http-route-workload.example.yaml` is held equal to this
/// by a test in this module, so the template gaining or losing a claim fails
/// here rather than in a cluster.
pub fn flow_http_identity_claims(identity: &DevActivationIdentity) -> [(&'static str, String); 5] {
    [
        ("wamn.env_key", identity.env_key()),
        ("wamn.catalog", identity.catalog.clone()),
        ("wamn.environment", identity.environment.clone()),
        ("wamn.project", identity.project.clone()),
        ("wamn.schema", identity.schema.clone()),
    ]
}

fn flow_http_request(request: &DevActivationRequest<'_>) -> v2::WorkloadStartRequest {
    let platform: serde_yaml::Value = serde_yaml::from_str(include_str!(
        "../../../../../deploy/platform/release/values.yaml"
    ))
    .expect("the platform chart defaults are valid YAML");
    let http = &platform["platform"]["roles"]["http"];
    let identity = request.identity;
    let config = flow_http_identity_claims(identity)
        .into_iter()
        .map(|(claim, value)| (claim.to_owned(), value))
        .collect::<HashMap<_, _>>();
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
            namespace: host_namespace(identity).to_owned(),
            name: FLOW_HTTP_NAME.to_owned(),
            annotations: HashMap::new(),
            service: None,
            wit_world: Some(v2::WitWorld {
                components: vec![v2::Component {
                    image: "flow-http.wasm".to_owned(),
                    local_resources: Some(local_resources),
                    pool_size: 0,
                    reclaim_window_seconds: 0,
                    reclaim_min_instances: 0,
                    max_invocations: 0,
                    image_pull_secret: None,
                    name: FLOW_HTTP_NAME.to_owned(),
                    image_pull_policy: v2::ImagePullPolicy::Always.into(),
                    max_concurrency: serde_yaml::from_value(http["maxConcurrency"].clone())
                        .expect("the HTTP store concurrency is an i32"),
                }],
                host_interfaces: vec![
                    v2::WitInterface {
                        namespace: "wasi".to_owned(),
                        package: "http".to_owned(),
                        version: "0.3.0".to_owned(),
                        interfaces: vec!["handler".to_owned()],
                        config: HashMap::from([(
                            "host".to_owned(),
                            request.config.route_host().to_owned(),
                        )]),
                        name: String::new(),
                    },
                    wit_interface("wamn", "flow-http-routing", "0.1.0", "routing"),
                    wit_interface("wamn", "router-delivery", "0.4.0", "delivery"),
                    wit_interface("wamn", "operation", "0.1.0", "operation"),
                    wit_interface("wamn", "postgres", "0.4.0", "statements"),
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

        // The host starts the materializer after flow-http runs, so its
        // wait has a deadline of its own.
        let deadline = Instant::now() + HOST_HEARTBEAT_TIMEOUT;
        loop {
            let response: v2::WorkloadStatusResponse = request_json(
                &mut backend,
                &rpc_subject(selected, "workload.status"),
                &v2::WorkloadStatusRequest {
                    workload_id: MATERIALIZER_WORKLOAD_ID.to_owned(),
                },
                "status-local-materializer",
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
                    "status-local-materializer",
                    response.workload_status,
                    MATERIALIZER_WORKLOAD_ID,
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
        && heartbeat.environment == host_namespace(identity)
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
            DevActivationErrorType::ProtocolViolation,
            step,
            detail,
            source,
        )
    })
}

/// Whether a polled workload may still reach RUNNING. The host reports
/// NOT_FOUND until its own preload finishes and it starts the workload; any
/// other state but STARTING is final.
fn workload_pending(status: Option<&v2::WorkloadStatus>) -> bool {
    status.is_none_or(|status| {
        [
            v2::WorkloadState::Unspecified,
            v2::WorkloadState::Starting,
            v2::WorkloadState::NotFound,
        ]
        .into_iter()
        .any(|state| status.workload_state == state as i32)
    })
}

fn require_running(
    step: &'static str,
    status: Option<v2::WorkloadStatus>,
    workload_id: &str,
) -> Result<(), DevActivationError> {
    let status = status.ok_or_else(|| {
        DevActivationError::new(
            DevActivationErrorType::ProtocolViolation,
            step,
            "response omitted workload_status",
        )
    })?;
    if status.workload_id != workload_id {
        return Err(DevActivationError::new(
            DevActivationErrorType::ProtocolViolation,
            step,
            format!(
                "response named workload {:?}, expected {workload_id:?}",
                status.workload_id
            ),
        ));
    }
    let state = v2::WorkloadState::try_from(status.workload_state).map_err(|_| {
        DevActivationError::new(
            DevActivationErrorType::ProtocolViolation,
            step,
            format!(
                "response carried unknown workload state {}",
                status.workload_state
            ),
        )
    })?;
    if state != v2::WorkloadState::Running {
        return Err(DevActivationError::new(
            DevActivationErrorType::WorkloadRefused,
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

fn require_stopped(
    status: Option<v2::WorkloadStatus>,
    workload_id: &str,
) -> Result<(), DevActivationError> {
    let status = status.ok_or_else(|| {
        DevActivationError::new(
            DevActivationErrorType::ProtocolViolation,
            "stop-workload",
            "response omitted workload_status",
        )
    })?;
    if status.workload_id != workload_id {
        return Err(DevActivationError::new(
            DevActivationErrorType::ProtocolViolation,
            "stop-workload",
            "response named a different workload",
        ));
    }
    let state = v2::WorkloadState::try_from(status.workload_state).map_err(|_| {
        DevActivationError::new(
            DevActivationErrorType::ProtocolViolation,
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
            DevActivationErrorType::CleanupFailed,
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
        DevActivationErrorType::SchedulerUnavailable,
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

    fn test_applications() -> wamn_catalog::EnvironmentApplications {
        serde_json::from_value(serde_json::json!({"receiving": {"packages": ["base", "overlay"]}}))
            .unwrap()
    }

    static HOST_LOG_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    struct HostLogFixture(PathBuf);

    impl Drop for HostLogFixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).expect("remove host log fixture");
        }
    }

    fn host_log_fixture() -> HostLogFixture {
        let sequence = HOST_LOG_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("wamn-host-log-{}-{sequence}", std::process::id()));
        std::fs::create_dir(&root).expect("create private host log fixture");
        HostLogFixture(root)
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
            "target_privileges_file": "/run/wamn-dev/privileges.sql",
            "target_template_database": "target--template",
            "target_database_acl_file": "/run/wamn-dev/database-acl.sql",
            "system_database_url": "postgresql://owner:owner-secret@127.0.0.1:41003/system",
            "identity_database_url": "postgresql://identity:identity-secret@127.0.0.1:41004/system",
            "guest_database_url": "postgresql://guest:guest-secret@127.0.0.1:41005/target",
            "executor_platform_database_url": "postgresql://platform:platform-secret@127.0.0.1:41006/target",
            "http_admitter_database_url": "postgresql://admitter:admitter-secret@127.0.0.1:41007/target",
            "event_materializer_database_url": "postgresql://materializer:materializer-secret@127.0.0.1:41008/target",
            "administration_database_url": "postgresql://administration:administration-secret@127.0.0.1:41013/target",
            "scheduler_nats_url": "nats://127.0.0.1:41009",
            "event_nats_url": "nats://127.0.0.1:41010",
            "event_nats_username": "dev_runtime",
            "event_nats_password_file": "/run/secrets/event-nats-password",
            "stream_replicas": 1,
            "dup_window_secs": 120,
            "otel_exporter_otlp_endpoint": "http://127.0.0.1:41016",
            "gate_bearer_token": "gate-secret",
            "route_host": "fixture.localhost",
            "platform_domain": "example.invalid",
            "local_artifacts": {
                "directory": "/tmp/wamn-local-candidate", "flow_http_component": "/tmp/flow-http.wasm",
                "materializer_component": "/tmp/materializer.wasm"
            },
            "package_sources": [],
            "catalog": "default",
            "environment": "fixture-dev",
            "org": "example",
            "project": "fixture",
            "schema": "inventory",
            "host_group": "wamn-dev-fixture",
            "host_name": "wamn-dev-fixture-1",
            "runner": "wamn-dev-fixture-1",
            "host_binary": "/opt/wamn/bin/wamn-host",
            "wasmtime_cache_dir": "/tmp/wamn-dev-cache",
            "materializer_nats_binding_file": "/tmp/wamn-dev/materializer-nats.json"
        })
    }

    fn identity() -> DevActivationIdentity {
        DevActivationIdentity {
            catalog: "default".to_owned(),
            environment: "fixture-dev".to_owned(),
            org: "example".to_owned(),
            project: "fixture".to_owned(),
            schema: "inventory".to_owned(),
            host_group: "wamn-dev-fixture".to_owned(),
            host_name: "wamn-dev-fixture-1".to_owned(),
            runner: "wamn-dev-fixture-1".to_owned(),
        }
    }

    /// The deploy template and the production path must declare the SAME
    /// identity claims. Either side gaining or losing one fails here.
    ///
    /// Read from the template rather than restated, and compared against the
    /// producer rather than a second list, so this cannot drift into the third
    /// hand-maintained copy of the thing it exists to prevent.
    #[test]
    fn the_workload_template_declares_exactly_the_claims_activation_sets() {
        let template = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../deploy/platform/http-route-workload.example.yaml"),
        )
        .expect("the workload template is checked in beside the platform manifests");

        let mut declared: Vec<&str> = template
            .lines()
            .filter_map(|line| line.trim().split_once(':'))
            .map(|(key, _)| key)
            .filter(|key| key.starts_with("wamn."))
            .collect();
        declared.sort_unstable();

        let identity = identity();
        let mut set: Vec<&str> = flow_http_identity_claims(&identity)
            .into_iter()
            .map(|(claim, _)| claim)
            .collect();
        set.sort_unstable();

        assert_eq!(
            declared, set,
            "deploy/platform/http-route-workload.example.yaml declares {declared:?} \
             but activation sets {set:?}; the cluster journey substitutes what the \
             template declares, so a claim in one and not the other deploys a \
             placeholder identity"
        );
    }

    /// Every claim carries a DISTINCT field. A producer that paired two claims
    /// to the same field would satisfy the set-equality test above while
    /// deploying one identity under another's name.
    #[test]
    fn each_identity_claim_carries_its_own_field() {
        let identity = DevActivationIdentity {
            catalog: "catalog-value".to_owned(),
            environment: "environment-value".to_owned(),
            org: "org-value".to_owned(),
            project: "project-value".to_owned(),
            schema: "schema-value".to_owned(),
            host_group: "host-group-value".to_owned(),
            host_name: "host-name-value".to_owned(),
            runner: "runner-value".to_owned(),
        };
        for (claim, value) in flow_http_identity_claims(&identity) {
            let field = claim
                .strip_prefix("wamn.")
                .expect("every claim is namespaced");
            assert_eq!(
                value,
                if field == "env_key" {
                    "org-value--project-value--environment-value".to_owned()
                } else {
                    format!("{field}-value")
                },
                "claim {claim} carries the wrong field of the identity"
            );
        }
    }

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

    /// Render locally with Helm: no cluster, release install, or registry access.
    #[test]
    fn local_host_matches_the_deployed_host_contract() {
        use crate::release_chart;
        use serde::Deserialize as _;
        use serde_yaml::Value;
        use std::collections::BTreeMap;

        // Compare effective settings, including flags with equivalent env fallbacks.
        fn settings(env: &[(String, String)], args: &[String]) -> BTreeMap<String, String> {
            assert_eq!(args.iter().filter(|arg| arg.as_str() == "host").count(), 1);
            let mut result = BTreeMap::new();
            for (key, value) in env {
                assert!(
                    result.insert(key.clone(), value.clone()).is_none(),
                    "duplicate env {key}"
                );
            }
            let mut args = args.iter();
            while let Some(arg) = args.next() {
                if arg == "host" {
                    continue;
                }
                let (key, value) = arg
                    .split_once('=')
                    .unwrap_or_else(|| (arg, args.next().expect("flag value")));
                let key = match key {
                    "--org" => "WAMN_ORG",
                    "--project" => "WAMN_PROJECT",
                    "--schema" => "WAMN_SCHEMA",
                    "--drain-bound-seconds" => "WAMN_DRAIN_BOUND_SECONDS",
                    "--wasmtime-cache-dir" => "WAMN_WASMTIME_CACHE_DIR",
                    "--materializer-nats-binding-file" => "WAMN_MAT_NATS_BINDING_FILE",
                    _ => key,
                };
                // The release's --environment overrides the chart's namespace argument.
                result.insert(key.to_owned(), value.to_owned());
            }
            result
        }

        let directory = host_log_fixture();
        let chart = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/platform/release");
        let digest = format!("sha256:{}", "7".repeat(64));
        let chart = release_chart::stamp(
            &chart,
            &release_chart::ImageSet {
                host: format!("local/host:test@{digest}"),
                http: format!("local/http@{digest}"),
                materializer: format!("local/materializer@{digest}"),
            },
            None,
            &directory.0.join("chart"),
        )
        .unwrap();
        let (org, project, environment) = ("dkk", "receiving", "dev");
        let group_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../deploy/gcp/host-group-receiving.yaml");
        let input = release_chart::ValuesInput {
            org: org.into(),
            project: project.into(),
            env: environment.into(),
            manifest_digest: digest.clone(),
            artifact_base: "local/releases".into(),
            route_host: "receiving.localhost".into(),
            applications: test_applications(),
            roles: vec![release_chart::Role {
                entry: Some(format!("entries@sha256:{}", "a".repeat(64))),
                statements: Vec::new(),
                name: release_chart::RoleName::Http,
                config: serde_yaml::Mapping::from_iter([(
                    "wamn.env_key".into(),
                    "parity-test".into(),
                )]),
                environment: None,
            }],
            // The fixture environment policy allows a 70-second drain.
            drain_bound_seconds: 70,
            actor: "parity-test".into(),
            host_group: crate::environment::write::host_group(
                Some(&group_path),
                org,
                project,
                environment,
            )
            .unwrap(),
        };
        let values = directory.0.join("values.yaml");
        std::fs::write(
            &values,
            serde_yaml::to_string(&release_chart::values(&input).unwrap()).unwrap(),
        )
        .unwrap();
        let output = std::process::Command::new("helm")
            .arg("template")
            .arg("parity-test")
            .arg(&chart)
            .args(["--namespace", "platform", "-f"])
            .arg(&values)
            .output()
            .expect("Helm must be installed for the host parity test");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let documents: Vec<Value> = serde_yaml::Deserializer::from_slice(&output.stdout)
            .map(|document| Value::deserialize(document).unwrap())
            .collect();
        let deployments: Vec<_> = documents
            .iter()
            .filter(|document| document["kind"].as_str() == Some("Deployment"))
            .collect();
        assert_eq!(deployments.len(), 1);
        let host = &deployments[0]["spec"]["template"]["spec"]["containers"][0];
        let deployed_env = host["env"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry["name"].as_str().unwrap().to_owned(),
                    entry["value"].as_str().map_or_else(
                        || serde_json::to_string(&entry["valueFrom"]).unwrap(),
                        str::to_owned,
                    ),
                )
            })
            .collect::<Vec<_>>();
        let deployed_args = host["args"]
            .as_sequence()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<Vec<_>>();

        let mut document = config_document();
        document["org"] = input.org.into();
        document["project"] = input.project.into();
        document["environment"] = input.env.into();
        document["schema"] = "receiving".into();
        document["session_identity"] = json!({
            "issuer": "https://127.0.0.1:8443", "ca": "/tmp/identity-ca.pem", "instance_suffix": "abc12345"
        });
        let config = parse_config(&serde_json::to_vec(&document).unwrap()).unwrap();
        let identity = config.activation_identity();
        let release = release();
        let request = DevActivationRequest {
            applications: &test_applications(),
            config: &config,
            release: &release,
            identity,
            host_binary: config.host_binary(),
            wasmtime_cache_dir: config.wasmtime_cache_dir(),
        };
        let local = host_process_spec(&request);
        let workload = flow_http_request(&request).workload.unwrap();
        let world = workload.wit_world.unwrap();
        let component = &world.components[0];
        let deployed_workload = documents
            .iter()
            .find(|document| document["kind"].as_str() == Some("WorkloadDeployment"))
            .expect("the deployed HTTP workload");
        let deployed_component = &deployed_workload["spec"]["template"]["spec"]["components"][0];
        for (field, local) in [
            ("poolSize", component.pool_size),
            ("maxConcurrency", component.max_concurrency),
            ("maxInvocations", component.max_invocations),
            ("reclaimWindowSeconds", component.reclaim_window_seconds),
            ("reclaimMinInstances", component.reclaim_min_instances),
        ] {
            assert_eq!(
                deployed_component[field].as_i64().unwrap_or_default(),
                i64::from(local),
                "HTTP store setting {field} differs"
            );
        }

        let mut deployed = settings(&deployed_env, &deployed_args);
        let mut local = settings(&local.env, &local.args);
        // Exact names only: new settings remain visible and fail the comparison.
        for key in [
            "WASMCLOUD_HOST_IP",              // Kubernetes supplies the pod IP.
            "WASMCLOUD_HOST_ENVIRONMENT", // Local workloads use the session environment, not a pod namespace.
            "--host-name", // Local sessions choose a process identity instead of a pod IP.
            "--host-group", // Local sessions choose an isolated host group.
            "--runner",    // Local sessions choose an isolated runner identity.
            "--http-addr", // Local HTTP binds a free loopback port.
            "--probe-addr", // Local probes bind a free loopback port.
            "--scheduler-nats-url", // Local NATS has a disposable loopback endpoint.
            "--data-nats-url", // Local activation does not use the deployed data-plane endpoint.
            "--scheduler-nats-tls-ca", // Local scheduler NATS uses plain TCP.
            "--scheduler-nats-tls-cert", // Local scheduler NATS uses plain TCP.
            "--scheduler-nats-tls-key", // Local scheduler NATS uses plain TCP.
            "--data-nats-tls-ca", // Local data-plane NATS uses plain TCP.
            "--data-nats-tls-cert", // Local data-plane NATS uses plain TCP.
            "--data-nats-tls-key", // Local data-plane NATS uses plain TCP.
            "--oci-ca-path", // Local release bytes require no registry TLS.
            "--oci-cache-dir", // Local release bytes require no OCI cache.
            "--release-artifact-base", // Deployed releases use a registry.
            "--release-manifest-digest", // Deployed releases use a registry.
            "--local-application", // Local releases use staged files.
            "--local-application-digest", // Local releases use staged files.
            "DOCKER_CONFIG", // Local releases require no registry credentials.
            "WAMN_COMPONENT_ARTIFACT_BASE", // Local components use staged files.
            "WAMN_REGISTRY_TOKEN_METADATA", // Local releases require no registry token.
            "WAMN_WASMTIME_CACHE_DIR", // Local sessions own their cache directory.
            "WAMN_PG_URL", // Local databases use disposable credentials and endpoints.
            "WAMN_SYSTEM_URL", // Local databases use disposable credentials and endpoints.
            "WAMN_EXECUTOR_PLATFORM_PG_URL", // Local databases use disposable credentials and endpoints.
            "WAMN_HTTP_ADMITTER_PG_URL", // Local databases use disposable credentials and endpoints.
            "WAMN_EVENT_MATERIALIZER_PG_URL", // Local databases use disposable credentials and endpoints.
            "WAMN_ADMINISTRATION_PG_URL", // Local databases use disposable credentials and endpoints.
            "WAMN_EVT_NATS_URL",          // Local events use a disposable broker endpoint.
            "WAMN_EVT_NATS_USERNAME",     // Local events use disposable broker credentials.
            "WAMN_EVT_NATS_PASSWORD_FILE", // Local event credentials use a private file.
            "WAMN_MAT_NATS_BINDING_FILE", // Local materializer credentials use a private file.
            "WAMN_CONNECTION_STORE_URL",  // Local credentials use the database reader.
            "WAMN_SESSION_ISSUER",        // Local identity uses a disposable issuer.
            "WAMN_SESSION_JWKS_CA",       // Local identity uses its private TLS CA.
            "WAMN_SESSION_INSTANCE_SUFFIX", // Local identity uses its provisioned instance.
            "OTEL_EXPORTER_OTLP_ENDPOINT", // The local collector has a loopback endpoint.
        ] {
            let deployed = deployed.remove(key);
            let local = local.remove(key);
            assert_ne!(deployed, local, "remove stale exception {key}");
        }
        assert_eq!(deployed, local, "unapproved host contract difference");
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
    async fn local_activation_uses_native_startup_without_registry_requests() {
        let fixture = host_log_fixture();
        let source = fixture.0.join("flow-http-source.wasm");
        let materializer = fixture.0.join("materializer-source.wasm");
        let directory = fixture.0.join("candidate");
        let mut document = config_document();
        document["local_artifacts"] = json!({"directory": directory, "flow_http_component": source,
            "materializer_component": materializer});
        let config = parse_config(&serde_json::to_vec(&document).unwrap()).unwrap();
        let mut resolve = wit_parser::Resolve::new();
        let package = resolve
            .push_str(
                "fixture.wit",
                "package wasi:http@0.3.0; interface handler {} world fixture { export handler; }",
            )
            .unwrap();
        let world = resolve.select_world(&[package], Some("fixture")).unwrap();
        let mut module =
            wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
        wit_component::embed_component_metadata(
            &mut module,
            &resolve,
            world,
            wit_component::StringEncoding::UTF8,
        )
        .unwrap();
        let component_bytes = wit_component::ComponentEncoder::default()
            .module(&module)
            .unwrap()
            .validate(true)
            .encode()
            .unwrap();
        std::fs::write(&source, &component_bytes).unwrap();
        let materializer_bytes = b"local materializer fixture";
        std::fs::write(&materializer, materializer_bytes).unwrap();
        std::fs::create_dir(&directory).unwrap();
        let admission = b"local-admission-fixture";
        std::fs::write(
            directory.join(wamn_runtime::local_application::LOCAL_FACTS_FILE),
            admission,
        )
        .unwrap();
        let identity = identity();
        let digest = wamn_engine::component_admission::component_digest(&component_bytes);
        let member = json!({"package-id": "fixture", "component": "entry", "digest": digest});
        let manifest: wamn_catalog::ServingManifest = serde_json::from_value(json!({
            "format-version": wamn_catalog::SERVING_MANIFEST_FORMAT_VERSION,
            "release": {"packages": [{"package-id": "fixture", "package-version": "1.0.0"}]},
            "components": [{"package-id": "fixture", "descriptor": {
                "component": "entry", "interface-version": "0.1", "component-digest": digest,
                "operations": {}, "imports": [], "imports-fingerprint": "", "effects": [], "connections": []
            }, "operations": {"fixture:item/get@1.0.0": {}}}],
            "routes": [], "attachments": {},
            "composed-entry": {
                "reference": format!("file:{}", wamn_engine::artifact_source::local_component_path(&directory, &digest).unwrap().display()),
                "route-digest": digest, "application": member, "members": [member]
            }
        })).unwrap();
        let manifest_bytes = manifest.canonical_bytes();
        let (_, manifest_digest) =
            wamn_catalog::ServingManifest::from_canonical_bytes(&manifest_bytes).unwrap();
        std::fs::write(
            directory.join(wamn_catalog::RELEASE_MANIFEST_FILE_NAME),
            manifest_bytes,
        )
        .unwrap();
        std::fs::write(
            wamn_engine::artifact_source::local_component_path(&directory, &digest).unwrap(),
            &component_bytes,
        )
        .unwrap();
        let release = ReleaseCarrier {
            artifact_base: format!("local:{}", directory.display()),
            manifest_digest,
        };
        let request = DevActivationRequest {
            applications: &test_applications(),
            config: &config,
            release: &release,
            identity: &identity,
            host_binary: config.host_binary(),
            wasmtime_cache_dir: config.wasmtime_cache_dir(),
        };
        prepare_local(&request).expect("Release stages validated workload bytes");
        let staged_workload = std::fs::read(directory.join("flow-http.json")).unwrap();
        let staged_http: v2::WorkloadStartRequest =
            serde_json::from_slice(&staged_workload).unwrap();
        let claims = staged_http
            .workload
            .unwrap()
            .wit_world
            .unwrap()
            .components
            .remove(0)
            .local_resources
            .unwrap()
            .config;
        assert_eq!(
            claims["wamn.env_key"].as_bytes(),
            b"example--fixture--fixture-dev"
        );
        let staged_materializer: v2::WorkloadStartRequest =
            serde_json::from_slice(&std::fs::read(directory.join("materializer.json")).unwrap())
                .unwrap();
        assert_eq!(staged_materializer.workload_id, MATERIALIZER_WORKLOAD_ID);
        let staged = staged_materializer.workload.unwrap();
        assert_eq!(staged.name, format!("{}-materializer", identity.project));
        let service = staged.service.unwrap();
        let materializer_digest =
            wamn_engine::component_admission::component_digest(materializer_bytes);
        assert_eq!(service.image, materializer_digest);
        assert_eq!(service.image_pull_policy, v2::ImagePullPolicy::Never as i32);
        assert_eq!(
            std::fs::read(
                wamn_engine::artifact_source::local_component_path(
                    &directory,
                    &materializer_digest
                )
                .unwrap()
            )
            .unwrap(),
            materializer_bytes
        );
        let resources = service.local_resources.unwrap();
        assert_eq!(
            resources.config["wamn.postgres.authority"],
            "event-materializer"
        );
        assert_eq!(
            resources.config["wamn.env_key"].as_bytes(),
            b"example--fixture--fixture-dev"
        );
        assert_eq!(resources.environment["WAMN_MAT_MAX_DELIVER"], "5");
        assert_eq!(
            resources.environment["WAMN_MAT_FETCH_MS"],
            wamn_control_provision::materializer_workload::FETCH_MS.to_string()
        );
        let world = staged.wit_world.unwrap();
        assert!(world.components.is_empty());
        assert!(world.host_interfaces.iter().any(|interface| {
            interface.namespace == "wasmcloud"
                && interface.package == "nats"
                && interface.name == "events"
        }));
        let component_path = wamn_engine::artifact_source::local_component_path(
            &directory,
            &wamn_engine::component_admission::component_digest(&component_bytes),
        )
        .unwrap();
        std::fs::remove_file(&source).unwrap();
        let entry_reference = staged_http_reference(&request).unwrap();
        assert_eq!(
            entry_reference,
            format!("file:{}", component_path.display())
        );
        let spec = host_process_spec(&request);
        assert!(
            spec.args
                .iter()
                .any(|argument| argument == "--local-application")
        );
        assert!(spec.args.iter().all(|argument| {
            ![
                "--registry-auth-file",
                "--release-artifact-base",
                "--component-artifact-base",
            ]
            .contains(&argument.as_str())
        }));
        let shared = Arc::new(Shared::default());
        let backend = FakeBackend::new(
            Arc::clone(&shared),
            [heartbeat(
                "local-host",
                &identity.host_name,
                &identity.environment,
                &identity.host_group,
            )],
            [
                response(&v2::WorkloadStatusResponse {
                    workload_status: Some(status(v2::WorkloadState::Running)),
                }),
                response(&v2::WorkloadStatusResponse {
                    workload_status: Some(status(v2::WorkloadState::Running)),
                }),
                response(&v2::WorkloadStatusResponse {
                    workload_status: Some(status(v2::WorkloadState::Running)),
                }),
            ],
            [],
        );
        let active = activate_backend(request, backend).await.unwrap();
        assert_eq!(&*active.host_id, "local-host");
        assert!(
            shared
                .spec
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .env
                .iter()
                .all(|(key, _)| key != "WAMN_TRUSTED_WARM_COMPONENT_DIGESTS")
        );
        assert_eq!(
            std::fs::read(directory.join("flow-http.json")).unwrap(),
            staged_workload
        );
        assert_eq!(std::fs::read(component_path).unwrap(), component_bytes);
        assert!(!source.exists());
        let requests = shared.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .all(|(subject, _)| subject.ends_with("workload.status"))
        );
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
        let expected_digest = format!("sha256:{}", "7".repeat(64));
        let expected_args = [
            "host",
            "--host-group",
            "wamn-dev-fixture",
            "--scheduler-nats-url",
            "nats://127.0.0.1:41009",
            "--host-name",
            "wamn-dev-fixture-1",
            "--runner",
            "wamn-dev-fixture-1",
            "--environment",
            "fixture-dev",
            "--applications",
            r#"{"receiving":{"packages":["base","overlay"]}}"#,
            "--http-addr",
            "127.0.0.1:0",
            "--local-application",
            "/tmp/wamn-local-candidate",
            "--local-application-digest",
            expected_digest.as_str(),
            "--wasmtime-cache-dir",
            "/tmp/wamn-dev-cache",
            "--materializer-nats-binding-file",
            "/tmp/wamn-dev/materializer-nats.json",
            "--project",
            "fixture",
            "--org",
            "example",
            "--schema",
            "inventory",
            "--drain-bound-seconds",
            "70",
            "--drain-delay",
            "5s",
            "--nats-connect-timeout",
            "60s",
            "--socket-egress",
            "count",
            "--probe-addr",
            "127.0.0.1:0",
        ]
        .map(str::to_owned);
        assert_eq!(&*spec.args, &expected_args);
        assert_eq!(
            &*spec.env,
            &[
                (
                    "WASMCLOUD_HOST_ENVIRONMENT".to_owned(),
                    identity.environment.clone(),
                ),
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
                    "WAMN_ADMINISTRATION_PG_URL".to_owned(),
                    "postgresql://administration:administration-secret@127.0.0.1:41013/target"
                        .to_owned(),
                ),
                (
                    "WAMN_EVT_NATS_URL".to_owned(),
                    "nats://127.0.0.1:41010".to_owned(),
                ),
                (
                    "WAMN_EVT_NATS_USERNAME".to_owned(),
                    "dev_runtime".to_owned()
                ),
                (
                    "WAMN_EVT_NATS_PASSWORD_FILE".to_owned(),
                    "/run/secrets/event-nats-password".to_owned()
                ),
                ("WAMN_EVT_STREAM_REPLICAS".to_owned(), "1".to_owned()),
                ("WAMN_EVT_DUP_WINDOW_SECS".to_owned(), "120".to_owned()),
                ("WAMN_EVT_ORG".to_owned(), "example".to_owned()),
                ("WAMN_EVT_PROJECT".to_owned(), "fixture".to_owned()),
                ("WAMN_EVT_ENV".to_owned(), "fixture-dev".to_owned()),
                (
                    "WAMN_MATERIALIZER_DRAIN_BOUND_SECONDS".to_owned(),
                    "10".to_owned(),
                ),
                (
                    "OTEL_EXPORTER_OTLP_ENDPOINT".to_owned(),
                    "http://127.0.0.1:41016".to_owned(),
                ),
                ("OTEL_SERVICE_NAME".to_owned(), "wamn-host".to_owned()),
                (
                    "OTEL_TRACES_SAMPLER".to_owned(),
                    "parentbased_traceidratio".to_owned()
                ),
                ("OTEL_TRACES_SAMPLER_ARG".to_owned(), "0.1".to_owned()),
                ("HOME".to_owned(), "/tmp".to_owned()),
                (
                    "WASMTIME_POOLING_LINEAR_MEMORY_KEEP_RESIDENT".to_owned(),
                    "65536".to_owned(),
                ),
                (
                    "WASMTIME_POOLING_TABLE_KEEP_RESIDENT".to_owned(),
                    "4096".to_owned(),
                ),
                (
                    "WASMTIME_POOLING_ASYNC_STACK_KEEP_RESIDENT".to_owned(),
                    "8192".to_owned(),
                ),
                ("WAMN_PG_GUEST_POOL_MAX".to_owned(), "16".to_owned()),
                ("WAMN_PG_PLATFORM_POOL_MAX".to_owned(), "8".to_owned()),
                (
                    "WAMN_HTTP_ROUTE_IN_FLIGHT_LIMIT".to_owned(),
                    "64".to_owned()
                ),
                ("WASH_DEFAULT_HEAP_MEMORY".to_owned(), "256MiB".to_owned()),
                ("WASH_CORE_INSTANCES".to_owned(), "512".to_owned()),
                ("WASH_GUEST_MEMORY_MODE".to_owned(), "count".to_owned()),
                ("WASH_MAX_CONCURRENT_STARTS".to_owned(), "4".to_owned()),
            ]
        );

        let requests = shared
            .requests
            .lock()
            .expect("request lock is not poisoned");
        assert_eq!(
            requests.len(),
            4,
            "flow-http running, the materializer running, one status reply, then stop"
        );
        assert_eq!(
            requests
                .iter()
                .map(|(subject, _)| subject.as_str())
                .collect::<Vec<_>>(),
            [
                "runtime.host.selected-host-id.workload.status",
                "runtime.host.selected-host-id.workload.status",
                "runtime.host.selected-host-id.workload.status",
                "runtime.host.selected-host-id.workload.stop",
            ]
        );
        let polled: v2::WorkloadStatusRequest =
            serde_json::from_slice(&requests[1].1).expect("the second status request decodes");
        assert_eq!(polled.workload_id, MATERIALIZER_WORKLOAD_ID);
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
        drop(requests);

        assert_eq!(
            *shared.events.lock().expect("event lock is not poisoned"),
            vec![
                Event::Subscribe(format!("{OPERATOR_API_PREFIX}.heartbeat.*")),
                Event::Flush,
                Event::Spawn,
                Event::Heartbeat,
                Event::Heartbeat,
                Event::Request("runtime.host.selected-host-id.workload.status".to_owned()),
                Event::Request("runtime.host.selected-host-id.workload.status".to_owned()),
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
