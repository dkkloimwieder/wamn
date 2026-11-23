//! The `host` subcommand: a ClusterHost deployable by the runtime-operator
//! Helm chart. Arg surface mirrors what the chart's runtime deployment
//! template renders for `wash host` (charts/runtime-operator).

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use clap::Args;
use clap::builder::TypedValueParser as _;
use wash_runtime::engine::WasmProposal;
use wash_runtime::engine::guest_memory::GuestMemoryMode;
use wash_runtime::engine::host_memory::HostMemoryBudgets;
use wash_runtime::host::HostConfig;
use wash_runtime::host::http::{HostHandler as _, Ingress};
use wash_runtime::host::probes::{self, Liveness, ProbeState};
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin;
use wash_runtime::sockets::policy::EgressMode;
use wash_runtime::washlet::AttachedHostControl;

mod application_route;
mod commands;
mod credential_store;
mod files;
mod host_route;
mod platform_http;
mod route_refusal;
mod workflow;

use wamn_authentication::access_token::AccessTokenVerifier;
use wamn_authentication::authorization::Authorization;
use wamn_authentication::route_authentication::{
    EnvironmentStatusReader, PlatformRouteAuthenticator,
};
use wamn_engine::engine::{DEFAULT_CORE_INSTANCES, build_engine_with_host_memory};
use wamn_engine::ingress::IngressRouting;
use wamn_identity_client::nats::{SignHostUser, sign_host_user};
use wamn_operation::{OperationDatabase, OperationHost};
use wamn_postgres::WamnPostgres;

#[derive(Debug, Args)]
pub struct HostArgs {
    /// The host group label to assign to the host
    #[arg(long = "host-group", default_value = "default")]
    pub host_group: String,

    /// NATS URL for control-plane communications
    #[arg(long = "scheduler-nats-url")]
    pub scheduler_nats_url: String,

    /// Passed by the runtime-operator chart (runtime.natsConnectTimeoutSeconds).
    /// Not used: the first connect retries without a bound once the server is
    /// reachable-but-down (wamn-gdex).
    #[arg(long, default_value = "0s", value_parser = humantime::parse_duration)]
    pub nats_connect_timeout: Duration,

    /// Keep broker connections on the chart-configured endpoints.
    #[arg(long, default_value_t = true)]
    pub nats_ignore_discovered_servers: bool,

    /// Wait during cleanup after refusing new work so traffic removal can propagate.
    #[arg(long, default_value = "5s", value_parser = humantime::parse_duration)]
    pub drain_delay: Duration,

    #[arg(long = "scheduler-nats-tls-ca")]
    pub scheduler_nats_tls_ca: Option<PathBuf>,

    #[arg(long = "scheduler-nats-tls-first", default_value_t = false)]
    pub scheduler_nats_tls_first: bool,

    /// NATS URL of the event plane: `wamn:jetstream`, the auth memory and the
    /// connection memory connect here using its PLATFORM credentials
    /// (docs/plan/alignment.md R5, R8).
    #[arg(long = "data-nats-url")]
    pub data_nats_url: String,

    #[arg(long = "data-nats-tls-ca")]
    pub data_nats_tls_ca: Option<PathBuf>,

    #[arg(long = "data-nats-tls-first", default_value_t = false)]
    pub data_nats_tls_first: bool,

    #[arg(long = "data-nats-creds")]
    pub data_nats_creds: Option<PathBuf>,

    /// The host configuration file that the operator chart renders for the
    /// host group's `plugins` entries (docs/plan/alignment.md R6, R9).
    #[arg(long, default_value = "/etc/wasmcloud/host-config/config.yaml")]
    pub user_config: PathBuf,

    /// The pod namespace supplied by the runtime operator.
    #[arg(long)]
    pub environment: Option<String>,

    /// Installation settings supplied by the environment chart.
    #[arg(long)]
    pub settings: PathBuf,

    /// The host name to assign to the host (chart passes the pod IP)
    #[arg(long = "host-name")]
    pub host_name: Option<String>,

    /// Address for the workload HTTP server
    #[arg(long, default_value = "0.0.0.0:9191")]
    pub http_addr: SocketAddr,

    /// Address for native readiness and liveness probes.
    #[arg(long, default_value = "0.0.0.0:8081")]
    pub probe_addr: Option<SocketAddr>,

    /// Maximum simultaneous incoming HTTP connections.
    #[arg(long, env = "WASH_MAX_HTTP_INGRESS_CONNECTIONS")]
    pub max_http_ingress_connections: Option<usize>,

    /// Maximum workloads that the native host starts concurrently.
    #[arg(long, env = "WASH_MAX_CONCURRENT_STARTS", default_value = "4")]
    pub max_concurrent_starts: Option<usize>,

    #[arg(long = "tls-cert-path", requires = "tls_key_path")]
    pub tls_cert_path: Option<PathBuf>,

    #[arg(long = "tls-key-path", requires = "tls_cert_path")]
    pub tls_key_path: Option<PathBuf>,

    #[arg(long = "tls-ca-path")]
    pub tls_ca_path: Option<PathBuf>,

    /// Extra PEM CA bundles trusted when pulling from OCI registries: for a
    /// registry behind a private or in-cluster CA, which the compiled-in public
    /// roots do not cover. Applies to every pull this host makes — the
    /// ClusterHost's workload components and this process's digest-verified
    /// component source alike.
    ///
    /// Prefer this to `--allow-insecure-registries`, which does not relax
    /// verification but replaces it: that flag switches every registry to plain
    /// HTTP, so credentials travel in the clear and no certificate is validated.
    ///
    /// Named to match what the chart renders (`hostGroups[].ociCaPaths` →
    /// `--oci-ca-path`, charts/runtime-operator/templates/runtime/deployment.yaml).
    #[arg(long = "oci-ca-path", env = "WASH_OCI_CA_PATHS", value_delimiter = ',')]
    pub oci_ca_paths: Vec<PathBuf>,

    /// The directory to use for caching OCI artifacts
    #[arg(long = "oci-cache-dir", default_value = "/oci-cache")]
    pub oci_cache_dir: Option<PathBuf>,

    /// Extra wasm proposals to enable on the engine (comma-separated)
    #[arg(long = "wasm-proposal", value_delimiter = ',')]
    pub wasm_proposals: Vec<WasmProposal>,

    /// Total guest-memory budget, applied according to guest-memory-mode.
    /// The separate process memory limit remains enforced in either mode.
    #[arg(long = "max-guest-memory", env = "WASH_HOST_MAX_GUEST_MEMORY")]
    pub max_guest_memory: Option<String>,

    /// Largest linear memory a guest may allocate.
    #[arg(
        long = "default-heap-memory",
        env = "WASH_DEFAULT_HEAP_MEMORY",
        default_value = "256MiB"
    )]
    pub default_heap_memory: String,

    /// Core-instance slots reserved by Wasmtime's pooling allocator.
    #[arg(
        long = "core-instances",
        env = "WASH_CORE_INSTANCES",
        default_value_t = DEFAULT_CORE_INSTANCES
    )]
    pub core_instances: u32,

    /// Guest memory mode. Enforce refuses growth beyond the container-derived guest budget.
    #[arg(
        long,
        env = "WASH_GUEST_MEMORY_MODE",
        default_value = "enforce",
        value_parser = clap::builder::PossibleValuesParser::new(["count", "enforce"])
            .map(|mode| if mode == "enforce" { GuestMemoryMode::Enforce } else { GuestMemoryMode::Count })
    )]
    pub guest_memory_mode: GuestMemoryMode,

    /// Raw-socket egress mode. Enforce blocks traffic refused by policy.
    /// The chart renders this flag from `networking.socketEgress`.
    #[arg(
        long,
        default_value = "enforce",
        value_parser = clap::builder::PossibleValuesParser::new(["count", "enforce"])
            .map(|mode| if mode == "enforce" { EgressMode::Enforce } else { EgressMode::Count })
    )]
    pub socket_egress: EgressMode,
}

const IDENTITY_CA: &str = "/etc/identity-ca/ca.crt";
const OAUTH_KEY: &str = "/etc/wamn/oauth/key";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Settings {
    org: wamn_registry::OrgId,
    env: wamn_registry::EnvName,
    drain_bound_seconds: u64,
    identity_issuer: Option<String>,
    nats_url: String,
    insecure_registry: bool,
    oauth_encryption: Option<wamn_envelope::encryption::Configuration>,
    projects: BTreeMap<wamn_registry::ProjectId, wamn_postgres::HostProject>,
}

/// Native plugin declarations and mounted Secret catalogs from the host chart.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UserConfig {
    host: UserHost,
    #[serde(default)]
    secrets: std::collections::BTreeMap<String, UserSecret>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UserSecret {
    dir: PathBuf,
}

#[derive(serde::Deserialize)]
struct UserHost {
    #[serde(default)]
    plugins: Vec<UserPlugin>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UserPlugin {
    id: String,
    #[serde(default)]
    config: HashMap<String, String>,
    #[serde(default)]
    bindings: std::collections::BTreeMap<String, UserBinding>,
    #[serde(default)]
    workload_config: Option<String>,
    #[serde(default)]
    secret_from: Vec<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UserBinding {
    #[serde(default)]
    config: HashMap<String, String>,
}

const HOST_PLUGIN_SECRET_DIRECTORY: &str = "/etc/wasmcloud/host-plugin-secrets";

/// The operator's binding declarations from the chart's host configuration
/// file. A `wasmcloud:nats` binding that names no `servers` dials the data
/// connection with its credentials, as `wash host` does. A binding with its own
/// server uses its own credential and TLS settings.
fn plugin_bindings(args: &HostArgs, source: &str) -> anyhow::Result<plugin::PluginBindings> {
    let config: UserConfig =
        serde_yaml::from_str(source).context("decode the host configuration file")?;
    for entry in &config.host.plugins {
        for secret in &entry.secret_from {
            let source = config
                .secrets
                .get(secret)
                .context("plugin Secret is absent from host secrets catalog")?;
            anyhow::ensure!(
                (entry.id == wamn_operation::WAMN_POSTGRES_ID
                    || entry.id == plugin::wasmcloud_nats::PLUGIN_NATS_ID)
                    && source.dir == PathBuf::from(HOST_PLUGIN_SECRET_DIRECTORY).join(secret),
                "unsupported plugin Secret projection"
            );
        }
    }
    let mut bindings = plugin::PluginBindings::new();
    for entry in config.host.plugins {
        let policy = match entry.workload_config.as_deref() {
            None | Some("deny") => plugin::WorkloadConfigPolicy::Deny,
            Some("allow") => plugin::WorkloadConfigPolicy::Allow,
            Some("warn") => plugin::WorkloadConfigPolicy::Warn,
            Some(other) => anyhow::bail!("plugin {} has workloadConfig {other:?}", entry.id),
        };
        let mut set = plugin::PluginBindingSet::new(entry.id.clone())
            .with_base(entry.config.into())
            .with_workload_config(policy);
        for (name, binding) in entry.bindings {
            set = set.with_binding(name, binding.config.into());
        }
        if entry.id == plugin::wasmcloud_nats::PLUGIN_NATS_ID {
            let mut bundle = vec![("servers", args.data_nats_url.clone())];
            if let Some(ca) = &args.data_nats_tls_ca {
                bundle.push(("tls-ca", ca.display().to_string()));
            }
            if let Some(creds) = &args.data_nats_creds {
                let document = zeroize::Zeroizing::new(
                    std::fs::read_to_string(creds).context("read data NATS credentials")?,
                );
                bundle.push((
                    "inbox-prefix",
                    wamn_broker::nats::credentials_inbox(&document)?,
                ));
                bundle.push(("creds", creds.display().to_string()));
            }
            if args.data_nats_tls_first {
                bundle.push(("tls-first", "true".to_owned()));
            }
            set = set.with_default_bundle("servers", bundle);
        }
        bindings = bindings.with_plugin(set);
    }
    Ok(bindings)
}

/// Resolve the native wash-runtime memory settings carried by the host CLI.
fn host_memory(args: &HostArgs) -> anyhow::Result<HostMemoryBudgets> {
    for (name, value) in [
        ("max-guest-memory", args.max_guest_memory.as_deref()),
        (
            "default-heap-memory",
            Some(args.default_heap_memory.as_str()),
        ),
    ] {
        anyhow::ensure!(
            value.is_none_or(|value| !value.trim().is_empty()),
            "--{name} must not be blank"
        );
    }
    HostMemoryBudgets::resolve_strs(
        args.max_guest_memory.as_deref(),
        Some(&args.default_heap_memory),
        None,
        Some(args.core_instances),
    )
    .map_err(anyhow::Error::msg)
}

/// The tracing target of the scheduler client's events. The `wasmcloud:nats`
/// plugin logs the same event words, and only the target and the `client`
/// field tell the two lines apart (wamn-gdex).
const SCHEDULER_NATS_TARGET: &str = "wamn_host::scheduler_nats";

/// The commands the scheduler client buffers while it is disconnected; a full
/// buffer makes the caller wait. The async-nats 0.49 default, stated.
const SCHEDULER_NATS_BUFFER: usize = 2048;

/// The scheduler client and its `name`. Reconnecting is the client's own retry
/// (wamn-gdex): it never gives up, and it buffers at most
/// `SCHEDULER_NATS_BUFFER` commands while disconnected. The first connect is
/// one attempt. Only a server that is not reachable yet (an IO, timeout or DNS
/// error) hands the first connect to the client's own retry, so the host
/// starts while NATS is down; a refused credential or TLS setup fails startup,
/// because no probe would show a host that never connects. A `ClientError`
/// event carries only the error type, so each line also counts the failed
/// passes since the last connect; async-nats logs the cause of each attempt on
/// `async_nats::connector` at debug.
async fn connect_scheduler_nats(
    args: &HostArgs,
    name: &str,
    issuer: &str,
) -> anyhow::Result<async_nats::Client> {
    let key = nkeys::KeyPair::new_user();
    let request = SignHostUser {
        public_key: key.public_key(),
    };
    let signed = sign_host_user(issuer, Path::new(IDENTITY_CA), &request)
        .await
        .context("request scheduler NATS credentials")?;
    anyhow::ensure!(
        signed.inbox_prefix == wamn_registry::name::nats_user_inbox(&request.public_key),
        "identity returned an unexpected NATS inbox"
    );
    let mut opts = async_nats::ConnectOptions::new()
        .name(name)
        .jwt(signed.jwt, move |nonce| {
            std::future::ready(key.sign(&nonce).map_err(async_nats::AuthError::new))
        })
        .custom_inbox_prefix(signed.inbox_prefix)
        .max_reconnects(None)
        .client_capacity(SCHEDULER_NATS_BUFFER);
    opts = wamn_broker::nats::tls_options(opts, args.scheduler_nats_tls_ca.as_deref());
    if args.nats_ignore_discovered_servers {
        opts = opts.ignore_discovered_servers();
    }
    if args.scheduler_nats_tls_first {
        opts = opts.tls_first();
    }
    let client = name.to_owned();
    let state = Arc::new(std::sync::Mutex::new((None::<Instant>, 0_u64)));
    opts = opts.event_callback(move |event| {
        let client = client.clone();
        let state = Arc::clone(&state);
        async move {
            match event {
                async_nats::Event::SlowConsumer(sid) => tracing::warn!(
                    target: SCHEDULER_NATS_TARGET,
                    %client,
                    subscription = sid,
                    "NATS slow consumer: the subscription buffer overflowed and messages were dropped"
                ),
                async_nats::Event::Disconnected => tracing::warn!(
                    target: SCHEDULER_NATS_TARGET,
                    %client,
                    "disconnected from scheduler NATS; buffered operations will be retried"
                ),
                async_nats::Event::Connected => {
                    *state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                        (Some(Instant::now()), 0);
                    tracing::info!(target: SCHEDULER_NATS_TARGET, %client, "connected to scheduler NATS");
                }
                async_nats::Event::ClientError(err) => {
                    let (connected, failed_passes) = {
                        let mut state = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.1 += 1;
                        *state
                    };
                    tracing::warn!(
                        target: SCHEDULER_NATS_TARGET,
                        %client,
                        %err,
                        failed_passes,
                        since_connected = ?connected.map(|at| at.elapsed()),
                        "scheduler NATS client error"
                    );
                }
                async_nats::Event::ServerError(err) => tracing::warn!(
                    target: SCHEDULER_NATS_TARGET,
                    %client,
                    %err,
                    "scheduler NATS server error"
                ),
                other => tracing::debug!(
                    target: SCHEDULER_NATS_TARGET,
                    %client,
                    event = %other,
                    "scheduler NATS connection event"
                ),
            }
        }
    });

    wamn_broker::nats::connect(
        opts,
        &args.scheduler_nats_url,
        wamn_broker::nats::InitialConnect::RetryTransport,
    )
    .await
}

pub async fn run(args: HostArgs) -> anyhow::Result<()> {
    let startup_started = Instant::now();
    let descriptor_soft_limit = wash_runtime::host::quota::raise_descriptor_limit();
    tracing::info!(
        ?descriptor_soft_limit,
        default_max_connections = wash_runtime::host::quota::default_max_connections(),
        default_max_http_ingress_connections =
            wash_runtime::host::quota::default_max_http_ingress_connections(),
        "native descriptor limits initialized"
    );
    let settings: Settings = serde_json::from_slice(
        &tokio::fs::read(&args.settings)
            .await
            .with_context(|| format!("read host settings {}", args.settings.display()))?,
    )
    .context("decode host settings")?;
    anyhow::ensure!(
        args.scheduler_nats_url == settings.nats_url && args.data_nats_url == settings.nats_url,
        "host broker arguments must match the settings NATS URL"
    );
    anyhow::ensure!(
        args.environment
            .as_deref()
            .is_none_or(|namespace| namespace == settings.org.as_str()),
        "operator host namespace and settings org differ"
    );
    tracing::info!(org = %settings.org, env = %settings.env, "loaded host settings");
    let issuer = settings
        .identity_issuer
        .as_deref()
        .context("scheduler NATS signing requires an identity issuer")?;
    let pod_name = std::env::var("POD_NAME").context("read POD_NAME for the host ID")?;
    let mut host_builder = wash_runtime::host::HostBuilder::default().with_id(pod_name)?;

    // Trust roots are a property of the host, not of any one pull, so they are
    // installed once here rather than carried to every construction site. This
    // must run before anything can pull: it is what the ClusterHost's own
    // workload pulls consult, and it is the call that validates the bundles, so
    // a host pointed at an unreadable or unusable CA refuses here instead of
    // starting and rejecting every pull from the registry it was given.
    if !args.oci_ca_paths.is_empty() {
        wash_runtime::oci::set_extra_ca_certificates(&args.oci_ca_paths)
            .context("trust the configured OCI CA bundles")?;
    }

    let router_owner = std::env::var("HOSTNAME")
        .ok()
        .filter(|owner| !owner.is_empty())
        .unwrap_or_else(|| "wamn-host".to_owned());

    let scheduler_nats_client = connect_scheduler_nats(
        &args,
        args.host_name.as_deref().unwrap_or(&router_owner),
        issuer,
    )
    .await?;
    let host_memory = host_memory(&args)?;
    let engine = build_engine_with_host_memory(
        &args.wasm_proposals,
        host_memory,
        args.guest_memory_mode,
        args.socket_egress,
    )?;
    let source = tokio::fs::read_to_string(&args.user_config)
        .await
        .with_context(|| {
            format!(
                "read the host configuration file {}",
                args.user_config.display()
            )
        })?;
    let pools = Arc::new(WamnPostgres::from_host_config(settings.projects)?);
    let envelope = settings
        .oauth_encryption
        .as_ref()
        .map(|config| match config {
            wamn_envelope::encryption::Configuration::Local => {
                wamn_envelope::encryption::Envelope::from_file(Path::new(OAUTH_KEY))
            }
            wamn_envelope::encryption::Configuration::Gcp { .. } => {
                wamn_envelope::encryption::Envelope::open(config)
            }
        })
        .transpose()?
        .map(Arc::new);
    let database = Arc::new(OperationDatabase::new(Arc::clone(&pools)));
    let authorization = Arc::new(
        Authorization::new(Arc::clone(&pools), database)
            .with_credential_store(Box::new(credential_store::KubernetesCredentials::default()))
            .with_oauth_encryption(envelope),
    );
    let postgres = Arc::new(OperationHost::new(pools, Some(Arc::clone(&authorization))));
    let integration = Arc::new(wamn_connection::integrations::IntegrationBound::new(
        Arc::clone(&authorization) as _,
    ));
    let connection_http = Arc::new(wamn_http::ConnectionHttp::new(
        Arc::clone(&postgres.scopes),
        Arc::new(wamn_http::transport::HttpTransport::new()?),
    ));
    let blobstore = Arc::new(wamn_blobstore::plugin::WamnBlobstore::new(Arc::clone(
        &postgres,
    )));
    let files = Arc::new(files::Files::new(
        &postgres,
        Arc::clone(&blobstore),
        Arc::clone(&authorization),
    ));
    postgres.set_result_signer(
        &(Arc::clone(&files) as Arc<dyn wamn_application::result_signing::ResultSigner>),
    );
    let logging = Arc::new(plugin::wasi_logging::TracingLogger::default());
    let verifier = settings
        .identity_issuer
        .as_deref()
        .map(|issuer| {
            AccessTokenVerifier::new(
                issuer,
                &format!("{}/.well-known/jwks.json", issuer.trim_end_matches('/')),
                &wamn_registry::name::org_audience(&settings.org),
                &["EdDSA".into()],
                Path::new(IDENTITY_CA),
            )
        })
        .transpose()?;
    let authentication = Arc::new(
        PlatformRouteAuthenticator::new(Arc::clone(&authorization), verifier)
            .with_connection_cache(Arc::clone(&authorization) as _),
    );
    let host_config = HostConfig {
        allow_oci_insecure: settings.insecure_registry,
        oci_pull_timeout: Some(Duration::from_secs(30)),
        oci_cache_dir: args.oci_cache_dir.clone(),
        oci_ca_paths: args.oci_ca_paths.clone(),
    };
    let ingress_routing = IngressRouting::new(Arc::clone(&postgres.scopes))
        .with_authenticator(authentication as Arc<_>);
    let status: Arc<dyn wamn_application::ingress::EnvironmentStatus> =
        Arc::new(EnvironmentStatusReader::new(Arc::clone(&authorization)));
    let ingress_routing = Arc::new(ingress_routing.with_environment_status(status));
    let operations = Arc::new(wamn_engine::operation::WamnOperation::new(
        Arc::clone(&postgres) as Arc<dyn plugin::HostPlugin>,
        Arc::clone(&postgres.scopes),
    ));
    let workflow = Arc::new(workflow::Workflow::new(
        Arc::clone(&postgres),
        Arc::clone(&operations),
        settings.nats_url,
        args.data_nats_tls_first,
    ));
    let mut plugins: Vec<Arc<dyn plugin::HostPlugin>> = vec![
        Arc::new(
            plugin::wasi_config::DynamicConfig::builder()
                .copy_environment(true)
                .build(),
        ),
        logging,
        blobstore,
        integration,
        connection_http,
        Arc::new(plugin::wasi_otel::WasiOtel::default()),
        operations,
        Arc::<workflow::Workflow>::clone(&workflow),
        postgres,
        // wamn-0h0g.15.96: READER 3 of the loaded release. Route projection stays wholly
        // in-memory; a PAT-protected route additionally carries the targeted
        // identity reader and preloads exact grants from the existing
        // callable-HTTP project pool during authentication.
        Arc::<IngressRouting>::clone(&ingress_routing),
    ];

    plugins.push(Arc::new(
        plugin::wasmcloud_nats::WasmcloudNats::new()
            .with_ignore_discovered_servers(args.nats_ignore_discovered_servers)
            .with_memory_budget(host_memory.max_guest_memory)
            .with_lattice_prefixes(vec![
                format!("{}.", wash_runtime::washlet::HOST_API_PREFIX),
                format!("{}.", wash_runtime::washlet::OPERATOR_API_PREFIX),
            ]),
    ));
    let native_event_bindings = plugin_bindings(&args, &source)?;

    // Count the same plugins supplied to the native host, including the optional bridge.
    let cleanup_budget = host_cleanup_budget(plugins.len())?;
    for plugin in plugins {
        host_builder = host_builder.with_plugin(plugin)?;
    }
    host_builder = host_builder.with_plugin_bindings(native_event_bindings);
    let mut builder = host_builder
        .with_engine(engine)
        .with_config(host_config)
        .with_meters(Meters::new(MeterKind::Duration))
        .with_label("hostgroup", args.host_group.clone())
        .with_environment(settings.org.as_str());
    if let Some(host_name) = &args.host_name {
        builder = builder.with_hostname(host_name);
    }
    let silence_budget = wash_runtime::washlet::liveness_silence(
        wash_runtime::washlet::ClusterHostBuilder::default().heartbeat_interval(),
    );
    let liveness = Liveness::new(silence_budget);
    let probe_state = ProbeState::default().with_liveness(Arc::clone(&liveness));
    let (stop_http, stopping) = tokio::sync::watch::channel(false);
    let (ingress_connections, ingress_handler) = {
        // The route host lives only on the http workload's `config.host`
        // (R2), so the host expects no host before a workload binds one.
        let router = wamn_engine::expected_router::expected_host_router(None, stopping.clone());
        let mut ingress = Ingress::builder(router, args.http_addr);
        if let Some(max) = args.max_http_ingress_connections {
            ingress = ingress.max_connections(max);
        }
        if let (Some(cert), Some(key)) = (&args.tls_cert_path, &args.tls_key_path) {
            let mut tls = wash_runtime::host::http::TlsConfig::new(cert, key);
            if let Some(ca) = args.tls_ca_path.as_deref() {
                tls = tls.with_ca(ca);
            }
            ingress = ingress.tls(tls);
        }
        let server = Arc::new(ingress.build().await?);
        let http_handler: Arc<dyn wash_runtime::host::http::HostHandler> =
            Arc::new(platform_http::PlatformHttp::new(
                Arc::clone(&server) as Arc<dyn wash_runtime::host::http::HostHandler>,
                Arc::clone(&files),
                Arc::clone(&ingress_routing),
                workflow,
            ));
        let connections = server.connection_limit();
        probe_state.register(Arc::new(connections.clone()));
        builder = builder.with_http_handler(http_handler);
        (connections, server)
    };
    let probe_listener = match args.probe_addr {
        Some(address) => Some(probes::bind(address).await?),
        None => None,
    };

    let prepared_host = builder.build().context("failed to build native host")?;
    tracing::info!(
        "wamn-host starting (base plugins: wasi:config, wasi:logging, wasi:otel, wamn:postgres, wamn:ingress)"
    );
    let mut probe_tasks = tokio::task::JoinSet::new();
    if let Some(listener) = probe_listener {
        probe_tasks.spawn(probes::serve(
            listener,
            probe_state.clone(),
            std::future::pending(),
        ));
    }
    let native_host = prepared_host
        .start()
        .await
        .context("failed to start native host")?;
    let workload_drain = Arc::new(commands::WorkloadDrain::default());
    let mut control_builder =
        AttachedHostControl::builder(Arc::clone(&native_host), Arc::new(scheduler_nats_client))
            .with_host_group(args.host_group)
            .with_liveness(Arc::clone(&liveness))
            .with_handler(Arc::new(commands::Commands {
                drain: Arc::clone(&workload_drain),
            }));
    if let Some(starts) = args.max_concurrent_starts {
        control_builder = control_builder.with_max_concurrent_starts(starts);
    }
    let control = match control_builder.attach().await {
        Ok(control) => control,
        Err(error) => {
            Arc::clone(&native_host).stop().await?;
            return Err(error.context("attach host control"));
        }
    };
    let cleanup_host = Arc::clone(&native_host);
    let native_cleanup = async move {
        let control_result = control.shutdown().await;
        let host_result = cleanup_host.stop().await;
        control_result?;
        host_result
    };
    probe_state.started();
    tracing::info!(
        elapsed_ms = %startup_started.elapsed().as_millis(),
        cleanup_budget_secs = cleanup_budget.as_secs(),
        "wamn-host runtime startup completed"
    );
    let drain_bound_seconds = settings.drain_bound_seconds;
    let result = {
        // Polling cleanup requests shutdown. Observe the retained native state instead.
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let stopped = async {
            tokio::select! {
                signal = tokio::signal::ctrl_c() => signal.context("receive SIGINT"),
                _ = sigterm.recv() => Ok(()),
                error = wamn_engine::lifecycle::watch_liveness(&liveness, Some(&probe_state), silence_budget) => Err(error),
                () = ingress_connections.stopped() => Err(anyhow::anyhow!("native HTTP ingress stopped unexpectedly")),
                task = probe_tasks.join_next(), if !probe_tasks.is_empty() => {
                    Err(probe_listener_failure(task.as_ref()))
                }
            }
        };
        let draining_connections = &ingress_connections;
        let stop_ingress = async move {
            ingress_handler
                .stop()
                .await
                .context("stop HTTP admission")?;
            // Keep control reachable while operator stops drain workloads.
            // Component replies report Stopping until HTTP drains (R20).
            // Past the operator wait bound, the host drain goes on.
            if let Err(error) = workload_drain
                .stop(&native_host, Duration::from_secs(drain_bound_seconds))
                .await
            {
                tracing::warn!(%error, "workload drain");
            }
            workload_drain
                .stop_components(&native_host, draining_connections.drained())
                .await
        };
        stop_combined_after(
            stopped,
            stop_ingress,
            native_cleanup,
            stop_http,
            &probe_state,
            args.drain_delay,
            shutdown_budget(settings.drain_bound_seconds),
        )
        .await
    };
    // Abort and join auxiliary tasks even when the native command task skipped Host::stop.
    probe_tasks.abort_all();
    let probe_result = wamn_engine::lifecycle::bounded_cleanup(Duration::from_secs(1), async {
        while probe_tasks.join_next().await.is_some() {}
        Ok(())
    })
    .await;
    result.and(probe_result)
}

/// The time the auxiliary teardown after [`stop_combined_after`] may take:
/// the identity connection and the probe tasks, one second each.
const EXIT_RESERVE: Duration = Duration::from_secs(2);

/// The one budget of the shutdown sequence (R20): the drain bound, less the
/// auxiliary teardown that follows it, so the process exits before the pod's
/// `terminationGracePeriodSeconds`, which the release chart renders from the
/// same drain bound.
fn shutdown_budget(drain_bound_seconds: u64) -> Duration {
    Duration::from_secs(drain_bound_seconds).saturating_sub(EXIT_RESERVE)
}

fn probe_listener_failure(task: Option<&Result<(), tokio::task::JoinError>>) -> anyhow::Error {
    anyhow::anyhow!("native probe listener stopped unexpectedly: {task:?}")
}

// Native timeout accessors are private; this is the documented whole-seconds
// override and default used by Host::stop in the pinned upstream runtime.
fn host_cleanup_budget(plugin_count: usize) -> anyhow::Result<Duration> {
    let plugin_stop = std::env::var("WASH_PLUGIN_STOP_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5);
    cleanup_budget_for_plugins(plugin_count, Duration::from_secs(plugin_stop))
}

fn cleanup_budget_for_plugins(
    plugin_count: usize,
    plugin_stop: Duration,
) -> anyhow::Result<Duration> {
    let plugin_count = u32::try_from(plugin_count).context("too many host plugins")?;
    let fixed = wash_runtime::washlet::COMMAND_DRAIN_TIMEOUT
        + wash_runtime::washlet::COMMAND_ABORT_TIMEOUT
        // Native HTTP stop has no timeout; this caps its coordination overhead.
        + Duration::from_secs(2);
    plugin_stop
        .checked_add(Duration::from_secs(1))
        .and_then(|budget| budget.checked_mul(plugin_count))
        .and_then(|budget| budget.checked_add(fixed))
        .context("native plugin stop override exceeds the supported shutdown budget")
}

async fn stop_combined_after<I, N>(
    stopped: impl std::future::Future<Output = anyhow::Result<()>>,
    stop_ingress: I,
    native_cleanup: N,
    stop_http: tokio::sync::watch::Sender<bool>,
    probes: &ProbeState,
    drain_delay: Duration,
    shutdown_budget: Duration,
) -> anyhow::Result<()>
where
    I: std::future::Future<Output = anyhow::Result<()>>,
    N: std::future::Future<Output = anyhow::Result<()>>,
{
    tokio::pin!(stopped);
    let result = tokio::select! {
        result = &mut stopped => result,
    };
    // Set one deadline for ingress shutdown and native cleanup (R20).
    let deadline = tokio::time::Instant::now() + shutdown_budget;
    let remaining = || deadline.saturating_duration_since(tokio::time::Instant::now());
    // Mark the host unready and tell the router to stop accepting requests.
    probes.drain();
    let _ = stop_http.send(true);
    let propagation_delay = if result.is_ok() {
        drain_delay
    } else {
        Duration::ZERO
    };
    tracing::info!(
        budget_secs = shutdown_budget.as_secs(),
        "shutting down wamn-host"
    );
    let ingress_result = wamn_engine::lifecycle::bounded_cleanup(remaining(), stop_ingress).await;
    let cleanup_native =
        async { wamn_engine::lifecycle::bounded_cleanup(remaining(), native_cleanup).await };
    let (native_result, ()) = tokio::join!(
        cleanup_native,
        tokio::time::sleep(propagation_delay.min(remaining()))
    );
    let native_result = normalize_explicit_ingress_stop(native_result, ingress_result.is_ok());
    let cleanup_result = ingress_result.and(native_result);
    if let Err(error) = &cleanup_result {
        tracing::error!(%error, "combined host cleanup failed");
    }
    result.and(cleanup_result)
}

// The pinned native supervisor reports listener closure as a failure, including
// our explicit stop. Preserve every other cleanup failure and the original trigger.
fn normalize_explicit_ingress_stop(
    result: anyhow::Result<()>,
    explicitly_stopped: bool,
) -> anyhow::Result<()> {
    match result {
        Err(error)
            if explicitly_stopped
                && error.to_string()
                    == "HTTP ingress stopped accepting connections; the host can no longer serve traffic" =>
        {
            Ok(())
        }
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    #[derive(Debug, clap::Parser)]
    struct TestCli {
        #[command(flatten)]
        args: HostArgs,
    }

    /// Parse `arguments` with the chart's required settings and broker arguments.
    fn parse<I, T>(arguments: I) -> Result<TestCli, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString>,
    {
        let mut arguments: Vec<std::ffi::OsString> =
            arguments.into_iter().map(Into::into).collect();
        for prefix in ["--settings=", "--scheduler-nats-url=", "--data-nats-url="] {
            if !arguments
                .iter()
                .any(|argument| argument.to_string_lossy().starts_with(prefix))
            {
                let value = if prefix == "--settings=" {
                    "/settings.json"
                } else {
                    "nats://wamn-nats.platform.svc.cluster.local:4222"
                };
                arguments.push(format!("{prefix}{value}").into());
            }
        }
        TestCli::try_parse_from(arguments)
    }

    #[test]
    fn the_chart_binding_inherits_the_data_connection_and_keeps_its_grants() {
        use plugin::HostPlugin as _;

        let args = parse([
            "wamn-host",
            "--data-nats-url=nats://wamn-nats.platform.svc.cluster.local:4222",
            "--data-nats-tls-ca=/data-cert/ca.crt",
        ])
        .unwrap()
        .args;
        let source = r"
host:
  plugins:
    - id: wasmcloud-nats
      workloadConfig: deny
      bindings:
        events:
          config:
            inbox-prefix: _INBOX_wamn-env-fixture--prod--widgets
            stream-allow: EVT_7_fixture_4_prod_7_widgets
            subject-allow: evt.fixture.prod.widgets.>
";
        let bindings = plugin_bindings(&args, source).unwrap();
        let declared = bindings.for_plugin(plugin::wasmcloud_nats::PLUGIN_NATS_ID);
        let schema = plugin::wasmcloud_nats::binding_schema();
        let native = plugin::wasmcloud_nats::WasmcloudNats::new();
        native.validate_bindings(&declared).unwrap();
        let narrows = |key: &str, ceiling: &str, value: &str| native.narrows(key, ceiling, value);
        let resolve = |name: &str, overrides: &wash_runtime::wit::Config| {
            declared.resolve(name, overrides, &schema, &narrows)
        };
        let events = resolve("events", &wash_runtime::wit::Config::new()).unwrap();
        assert_eq!(
            events.get("servers"),
            Some("nats://wamn-nats.platform.svc.cluster.local:4222")
        );
        assert_eq!(events.get("tls-ca"), Some("/data-cert/ca.crt"));
        assert_eq!(
            events.get("stream-allow"),
            Some("EVT_7_fixture_4_prod_7_widgets")
        );
        assert!(resolve("foreign", &wash_runtime::wit::Config::new()).is_err());
        // R6: workloadConfig deny refuses a workload's connection or wider grant.
        for (key, value) in [
            ("servers", "nats://foreign.invalid:4222"),
            ("tls-ca", "/foreign/ca.crt"),
            ("inbox-prefix", "_INBOX_foreign"),
            ("stream-allow", ">"),
            ("subject-allow", ">"),
        ] {
            let overrides = [(key, value)].into_iter().collect();
            resolve("events", &overrides).unwrap_err();
        }
        assert!(
            plugin_bindings(&args, "host:\n  plugins:\n    - id: x\n      image: y\n").is_err()
        );
    }

    #[test]
    fn cleanup_budget_accounts_for_every_native_plugin_and_override() {
        assert_eq!(
            cleanup_budget_for_plugins(6, Duration::from_secs(5)).unwrap(),
            Duration::from_secs(45)
        );
        assert_eq!(
            cleanup_budget_for_plugins(7, Duration::from_secs(5)).unwrap(),
            Duration::from_secs(51)
        );
        assert_eq!(
            cleanup_budget_for_plugins(7, Duration::from_secs(30)).unwrap(),
            Duration::from_secs(226)
        );
        assert!(cleanup_budget_for_plugins(7, Duration::MAX).is_err());
    }

    /// The chart's native memory settings reach `HostMemoryBudgets` intact.
    #[test]
    fn the_native_memory_flags_reach_host_memory_budgets() {
        let arguments = [
            "wamn-host",
            "--guest-memory-mode=count",
            "--max-guest-memory",
            "512Mi",
            "--default-heap-memory",
            "64MiB",
            "--core-instances",
            "7",
        ];
        let mut cli = parse(arguments).expect("the native memory flags parse");
        assert_eq!(cli.args.guest_memory_mode, GuestMemoryMode::Count);
        assert_eq!(
            parse(["wamn-host", "--guest-memory-mode=enforce"])
                .unwrap()
                .args
                .guest_memory_mode,
            GuestMemoryMode::Enforce
        );
        assert_eq!(
            parse(["wamn-host"]).unwrap().args.guest_memory_mode,
            GuestMemoryMode::Enforce
        );
        assert!(parse(["wamn-host", "--guest-memory-mode=off"]).is_err());
        assert_eq!(cli.args.socket_egress, EgressMode::Enforce);
        assert_eq!(
            parse(["wamn-host", "--socket-egress=count"])
                .unwrap()
                .args
                .socket_egress,
            EgressMode::Count
        );
        assert!(parse(["wamn-host", "--socket-egress=off"]).is_err());
        assert_eq!(
            host_memory(&cli.args).expect("memory settings resolve"),
            HostMemoryBudgets {
                max_guest_memory: 512 << 20,
                default_heap_memory: 64 << 20,
                max_memories: 7,
                core_instances: 7,
            },
            "all native memory settings must reach the engine"
        );
        cli.args.max_guest_memory = Some("  ".to_owned());
        assert!(host_memory(&cli.args).is_err());
        cli.args.max_guest_memory = None;
        cli.args.default_heap_memory = "  ".to_owned();
        assert!(host_memory(&cli.args).is_err());
    }
}
