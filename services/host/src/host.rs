//! The `host` subcommand: a ClusterHost deployable by the runtime-operator
//! Helm chart. Arg surface mirrors what the chart's runtime deployment
//! template renders for `wash host` (charts/runtime-operator).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use clap::Args;
use clap::builder::TypedValueParser as _;
use wash_runtime::engine::WasmProposal;
use wash_runtime::engine::guest_memory::GuestMemoryMode;
use wash_runtime::engine::host_memory::HostMemoryBudgets;
use wash_runtime::host::HostConfig;
use wash_runtime::host::http::{ConnectionLimit, HostHandler as _, Ingress};
use wash_runtime::host::probes::{self, Liveness, ProbeState};
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin;
use wash_runtime::sockets::policy::EgressMode;
use wash_runtime::washlet::AttachedHostControl;

mod authentication;
mod commands;
mod files;
mod platform_http;
mod projects;

use wamn_authentication::route_authentication::EnvironmentStatusReader;
use wamn_engine::engine::{
    DEFAULT_CORE_INSTANCES, build_engine_with_host_memory,
    build_engine_with_host_memory_and_compilation_cache,
};
use wamn_engine::flow_http_routing::FlowHttpRouting;
use wamn_sql::WamnPostgres;

#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent command-line switch of the host"
)]
#[derive(Debug, Args)]
pub struct HostArgs {
    /// The host group label to assign to the host
    #[arg(long = "host-group", default_value = "default")]
    pub host_group: String,

    /// NATS URL for control-plane communications
    #[arg(long = "scheduler-nats-url", default_value = "nats://localhost:4222")]
    pub scheduler_nats_url: String,

    /// Passed by the runtime-operator chart (runtime.natsConnectTimeoutSeconds).
    /// Not used: the first connect retries without a bound once the server is
    /// reachable-but-down (wamn-gdex).
    #[arg(long, default_value = "0s", value_parser = humantime::parse_duration)]
    pub nats_connect_timeout: Duration,

    /// Wait during cleanup after refusing new work so traffic removal can propagate.
    #[arg(long, env = "WASH_DRAIN_DELAY", default_value = "0s", value_parser = humantime::parse_duration)]
    pub drain_delay: Duration,

    /// The environment policy's drain bound, which the release chart also
    /// renders as the pod's `terminationGracePeriodSeconds` (R20). One budget
    /// bounds the whole shutdown sequence after SIGTERM. It has no default: a
    /// host given none refuses to start.
    #[arg(long, env = "WAMN_DRAIN_BOUND_SECONDS")]
    pub drain_bound_seconds: u64,

    #[arg(long = "scheduler-nats-tls-ca")]
    pub scheduler_nats_tls_ca: Option<PathBuf>,

    #[arg(long = "scheduler-nats-tls-first", default_value_t = false)]
    pub scheduler_nats_tls_first: bool,

    #[arg(long = "scheduler-nats-tls-cert")]
    pub scheduler_nats_tls_cert: Option<PathBuf>,

    #[arg(long = "scheduler-nats-tls-key")]
    pub scheduler_nats_tls_key: Option<PathBuf>,

    /// NATS URL of the event plane: `wamn:jetstream`, the auth memory and the
    /// connection memory connect here with the environment certificate
    /// (docs/plan/alignment.md R5, R8).
    #[arg(long = "data-nats-url", default_value = "nats://localhost:4222")]
    pub data_nats_url: String,

    #[arg(long = "data-nats-tls-ca")]
    pub data_nats_tls_ca: Option<PathBuf>,

    #[arg(long = "data-nats-tls-first", default_value_t = false)]
    pub data_nats_tls_first: bool,

    #[arg(long = "data-nats-tls-cert")]
    pub data_nats_tls_cert: Option<PathBuf>,

    #[arg(long = "data-nats-tls-key")]
    pub data_nats_tls_key: Option<PathBuf>,

    /// The host configuration file that the operator chart renders for the
    /// host group's `plugins` entries (docs/plan/alignment.md R6, R9).
    #[arg(long = "user-config")]
    pub user_config: PathBuf,

    /// The address of the `wasmcloud:nats` bindings, when it is not the data
    /// connection. Left out, a binding that names no `servers` takes the data
    /// URL and its TLS (the chart's `wasmcloudNatsUrl`).
    #[arg(long = "wasmcloud-nats-url")]
    pub wasmcloud_nats_url: Option<String>,

    /// The host name to assign to the host (chart passes the pod IP)
    #[arg(long = "host-name")]
    pub host_name: Option<String>,

    /// Stable node-acquisition owner. Never derived from `--host-name`, whose
    /// chart value is a pod IP and is not a valid runner identity.
    #[arg(long, env = "WAMN_RUNNER")]
    pub runner: Option<String>,

    /// Address for the workload HTTP server
    #[arg(long = "http-addr")]
    pub http_addr: Option<SocketAddr>,

    /// Address for native readiness and liveness probes.
    #[arg(long, env = "WASH_PROBE_ADDR")]
    pub probe_addr: Option<SocketAddr>,

    /// Maximum simultaneous incoming HTTP connections.
    #[arg(long, env = "WASH_MAX_HTTP_INGRESS_CONNECTIONS")]
    pub max_http_ingress_connections: Option<usize>,

    /// Maximum workloads that the native host starts concurrently.
    #[arg(long, env = "WASH_MAX_CONCURRENT_STARTS")]
    pub max_concurrent_starts: Option<usize>,

    #[arg(long = "tls-cert-path", requires = "tls_key_path")]
    pub tls_cert_path: Option<PathBuf>,

    #[arg(long = "tls-key-path", requires = "tls_cert_path")]
    pub tls_key_path: Option<PathBuf>,

    #[arg(long = "tls-ca-path")]
    pub tls_ca_path: Option<PathBuf>,

    /// Allow insecure (HTTP) OCI registries — needed for the in-cluster dev registry
    #[arg(long = "allow-insecure-registries", default_value_t = false)]
    pub allow_insecure_registries: bool,

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
    #[arg(long = "oci-cache-dir")]
    pub oci_cache_dir: Option<PathBuf>,

    /// Host-private Wasmtime compiled-component cache.
    ///
    /// Keep this outside `--oci-cache-dir`: wash-runtime's OCI sweeper owns
    /// every child of that directory and may delete it while the host runs.
    #[arg(long = "wasmtime-cache-dir", env = "WAMN_WASMTIME_CACHE_DIR")]
    pub wasmtime_cache_dir: Option<PathBuf>,

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

    /// OAuth envelope provider as JSON: local, or gcp with project and region.
    #[arg(long, env = "WAMN_OAUTH_ENCRYPTION")]
    pub oauth_encryption: Option<String>,

    /// Trusted HTTPS issuer for session routes, never discovered from a token.
    #[arg(long, env = "WAMN_SESSION_ISSUER", requires = "session_jwks_ca")]
    pub session_issuer: Option<String>,

    /// Public CA bundle for the configured identity service.
    #[arg(long, env = "WAMN_SESSION_JWKS_CA", requires = "session_issuer")]
    pub session_jwks_ca: Option<PathBuf>,

    /// Optional application data search path installed at node checkout.
    #[arg(long, env = "WAMN_SCHEMA")]
    pub schema: Option<String>,

    /// Production outbound ceiling for connection-backed HTTP effects.
    #[arg(long, env = "WAMN_ALLOWED_HOSTS", value_delimiter = ',')]
    pub allowed_hosts: Vec<String>,
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

/// The operator's binding declarations from the chart's host configuration
/// file. A `wasmcloud:nats` binding that names no `servers` dials the data
/// connection with its certificate, as `wash host` does: the address and its
/// TLS are one bundle, so a binding pointed elsewhere inherits no TLS.
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
                entry.id == wamn_sql::WAMN_POSTGRES_ID
                    && source.dir
                        == PathBuf::from("/etc/wasmcloud/host-plugin-secrets").join(secret),
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
            let mut bundle = vec![(
                "servers",
                args.wasmcloud_nats_url
                    .clone()
                    .unwrap_or_else(|| args.data_nats_url.clone()),
            )];
            if args.wasmcloud_nats_url.is_none() {
                for (key, path) in [
                    ("tls-ca", &args.data_nats_tls_ca),
                    ("tls-cert", &args.data_nats_tls_cert),
                    ("tls-key", &args.data_nats_tls_key),
                ] {
                    if let Some(path) = path {
                        bundle.push((key, path.display().to_string()));
                    }
                }
                if args.data_nats_tls_first {
                    bundle.push(("tls-first", "true".to_owned()));
                }
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
async fn connect_scheduler_nats(args: &HostArgs, name: &str) -> anyhow::Result<async_nats::Client> {
    let mut opts = async_nats::ConnectOptions::new()
        .name(name)
        .max_reconnects(None)
        .client_capacity(SCHEDULER_NATS_BUFFER);
    opts = wamn_broker::nats::tls_options(
        opts,
        args.scheduler_nats_tls_ca.as_deref(),
        args.scheduler_nats_tls_cert.as_deref(),
        args.scheduler_nats_tls_key.as_deref(),
        None,
    )?;
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
    let args = Arc::new(args);
    let startup_started = Instant::now();
    let descriptor_soft_limit = wash_runtime::host::quota::raise_descriptor_limit();
    tracing::info!(
        ?descriptor_soft_limit,
        default_max_connections = wash_runtime::host::quota::default_max_connections(),
        default_max_http_ingress_connections =
            wash_runtime::host::quota::default_max_http_ingress_connections(),
        "native descriptor limits initialized"
    );
    wash_runtime::init_crypto();

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

    let router_owner = args
        .runner
        .clone()
        .filter(|owner| !owner.is_empty())
        .or_else(|| {
            std::env::var("HOSTNAME")
                .ok()
                .filter(|owner| !owner.is_empty())
        })
        .unwrap_or_else(|| "wamn-host".to_owned());

    let scheduler_nats_client =
        connect_scheduler_nats(&args, args.host_name.as_deref().unwrap_or(&router_owner)).await?;
    let host_memory = host_memory(&args)?;
    let engine = Arc::new(
        if let Some(compilation_cache_dir) = args.wasmtime_cache_dir.as_deref() {
            build_engine_with_host_memory_and_compilation_cache(
                &args.wasm_proposals,
                host_memory,
                args.guest_memory_mode,
                args.socket_egress,
                compilation_cache_dir,
            )?
        } else {
            build_engine_with_host_memory(
                &args.wasm_proposals,
                host_memory,
                args.guest_memory_mode,
                args.socket_egress,
            )?
        },
    );
    let source = tokio::fs::read_to_string(&args.user_config)
        .await
        .with_context(|| {
            format!(
                "read the host configuration file {}",
                args.user_config.display()
            )
        })?;
    let mut user_config: UserConfig =
        serde_yaml::from_str(&source).context("decode host configuration")?;
    let postgres_config = user_config
        .host
        .plugins
        .iter_mut()
        .find(|plugin| plugin.id == wamn_sql::WAMN_POSTGRES_ID)
        .map(|plugin| std::mem::take(&mut plugin.config))
        .unwrap_or_default();
    let projects = Arc::new(projects::Projects::default());
    let postgres = Arc::new(WamnPostgres::from_host_config(&postgres_config)?);
    postgres.set_authorization_store(Arc::new(authentication::Authorization::new(
        Arc::downgrade(&postgres),
    )));
    let blobstore = Arc::new(wamn_blobstore::plugin::WamnBlobstore::new(Arc::clone(
        &postgres,
    )));
    let files = Arc::new(files::Files::new(&postgres, Arc::clone(&blobstore)));
    postgres.set_result_signer(
        &(Arc::clone(&files) as Arc<dyn wamn_sql::result_signing::ResultSigner>),
    );
    let logging = Arc::new(plugin::wasi_logging::TracingLogger::default());
    let authentication = Arc::new(projects::Authentication(Arc::clone(&projects)));
    let host_config = HostConfig {
        allow_oci_insecure: args.allow_insecure_registries,
        oci_pull_timeout: Some(Duration::from_secs(30)),
        oci_cache_dir: args.oci_cache_dir.clone(),
        oci_ca_paths: args.oci_ca_paths.clone(),
    };
    let flow_http = FlowHttpRouting::new(Arc::clone(&postgres.scopes))
        .with_authenticator(authentication as Arc<_>);
    let status: Arc<dyn wamn_engine::flow_http_routing::EnvironmentStatus> =
        Arc::new(EnvironmentStatusReader::new(Arc::clone(&postgres)));
    let flow_http = Arc::new(flow_http.with_environment_status(status));
    let mut plugins: Vec<Arc<dyn plugin::HostPlugin>> = vec![
        Arc::new(
            plugin::wasi_config::DynamicConfig::builder()
                .copy_environment(true)
                .build(),
        ),
        logging,
        blobstore,
        Arc::new(plugin::wasi_otel::WasiOtel::default()),
        Arc::new(wamn_engine::operation::WamnOperation::new(
            Arc::clone(&postgres) as Arc<dyn plugin::HostPlugin>,
        )),
        Arc::<WamnPostgres>::clone(&postgres),
        // wamn-0h0g.15.96: READER 3 of the loaded release. Route projection stays wholly
        // in-memory; a PAT-protected route additionally carries the targeted
        // identity reader and preloads exact grants from the existing
        // callable-HTTP project pool during authentication.
        Arc::<FlowHttpRouting>::clone(&flow_http),
    ];

    plugins.push(Arc::new(
        plugin::wasmcloud_nats::WasmcloudNats::new()
            .with_memory_budget(host_memory.max_guest_memory)
            .with_lattice_prefixes(vec![
                format!("{}.", wash_runtime::washlet::HOST_API_PREFIX),
                format!("{}.", wash_runtime::washlet::OPERATOR_API_PREFIX),
            ]),
    ));
    let native_event_bindings = plugin_bindings(&args, &source)?;

    // Count the same plugins supplied to the native host, including the optional bridge.
    let cleanup_budget = host_cleanup_budget(plugins.len())?;
    let mut host_builder = wash_runtime::host::HostBuilder::default();
    for plugin in plugins {
        host_builder = host_builder.with_plugin(plugin)?;
    }
    host_builder = host_builder.with_plugin_bindings(native_event_bindings);
    let mut builder = host_builder
        .with_engine((*engine).clone())
        .with_config(host_config)
        .with_meters(Meters::new(MeterKind::Duration))
        .with_label("hostgroup", args.host_group.clone());
    if let Some(host_name) = &args.host_name {
        builder = builder.with_hostname(host_name);
    }
    if let Some(namespace) = host_namespace(std::env::var("WASMCLOUD_HOST_ENVIRONMENT").ok()) {
        builder = builder.with_environment(namespace);
    }
    let silence_budget = wash_runtime::washlet::liveness_silence(
        wash_runtime::washlet::ClusterHostBuilder::default().heartbeat_interval(),
    );
    let liveness = Liveness::new(silence_budget);
    let probe_state = ProbeState::default().with_liveness(Arc::clone(&liveness));
    let (stop_queue, stopping) = tokio::sync::watch::channel(false);
    let mut ingress_connections = None;
    let mut ingress_handler = None;
    if let Some(addr) = args.http_addr {
        // The route host lives only on the http workload's `config.host`
        // (R2), so the host expects no host before a workload binds one.
        let router = wamn_engine::expected_router::expected_host_router(None, stopping.clone());
        let mut ingress = Ingress::builder(router, addr);
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
                Arc::clone(&flow_http),
            ));
        let connections = server.connection_limit();
        probe_state.register(Arc::new(connections.clone()));
        ingress_connections = Some(connections);
        ingress_handler = Some(Arc::clone(&server));
        builder = builder.with_http_handler(http_handler);
    }
    let probe_listener = match args.probe_addr {
        Some(address) => Some(probes::bind(address).await?),
        None => None,
    };

    let prepared_host = builder.build().context("failed to build native host")?;
    tracing::info!(
        "wamn-host starting (base plugins: wasi:config, wasi:logging, wasi:otel, wamn:postgres, wamn:flow-http-routing)"
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
            .with_host_group(args.host_group.clone())
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
    let drain_bound_seconds = args.drain_bound_seconds;
    let result = {
        // Polling cleanup requests shutdown. Observe the retained native state instead.
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let stopped = async {
            tokio::select! {
                signal = tokio::signal::ctrl_c() => signal.context("receive SIGINT"),
                _ = sigterm.recv() => Ok(()),
                error = wamn_engine::lifecycle::watch_liveness(&liveness, Some(&probe_state), silence_budget) => Err(error),
                () = ingress_stopped(ingress_connections.as_ref()) => Err(anyhow::anyhow!("native HTTP ingress stopped unexpectedly")),
                task = probe_tasks.join_next(), if !probe_tasks.is_empty() => {
                    Err(probe_listener_failure(task.as_ref()))
                }
            }
        };
        let had_ingress = ingress_handler.is_some();
        let draining_connections = ingress_connections.clone();
        let stop_ingress = async move {
            if let Some(handler) = ingress_handler {
                handler.stop().await.context("stop HTTP admission")?;
            }
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
                .stop_components(&native_host, async {
                    if let Some(connections) = &draining_connections {
                        connections.drained().await;
                    }
                })
                .await
        };
        stop_combined_after(
            stopped,
            stop_ingress,
            had_ingress,
            native_cleanup,
            stop_queue,
            &probe_state,
            args.drain_delay,
            shutdown_budget(args.drain_bound_seconds),
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

/// The host namespace the heartbeat names: the pod namespace the operator
/// chart sets in `WASMCLOUD_HOST_ENVIRONMENT`, because the operator's
/// `allowSharedHosts: false` lock places a WorkloadDeployment only on hosts
/// of its namespace, which every environment's release shares (R6). It is
/// never the environment: `--environment` names the release target only.
fn host_namespace(chart: Option<String>) -> Option<String> {
    chart.filter(|namespace| !namespace.is_empty())
}

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

async fn ingress_stopped(connections: Option<&ConnectionLimit>) {
    match connections {
        Some(connections) => connections.stopped().await,
        None => std::future::pending().await,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Keep the single shutdown owner, both execution paths, and their shared budget explicit"
)]
async fn stop_combined_after<I, N>(
    stopped: impl std::future::Future<Output = anyhow::Result<()>>,
    stop_ingress: I,
    had_ingress: bool,
    native_cleanup: N,
    stop_queue: tokio::sync::watch::Sender<bool>,
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
    // One deadline for the whole sequence, set when the shutdown trigger
    // arrives (R20). Ingress stop, queue drain and native cleanup share what
    // remains of it.
    let deadline = tokio::time::Instant::now() + shutdown_budget;
    let remaining = || deadline.saturating_duration_since(tokio::time::Instant::now());
    // This is the single admission cut: readiness drains before either HTTP
    // cleanup or queue cleanup can wait. The expected router reads the same
    // stop signal and admits no new request; the queue turns to its drain.
    probes.drain();
    let _ = stop_queue.send(true);
    let propagation_delay = if result.is_ok() {
        drain_delay
    } else {
        Duration::ZERO
    };
    tracing::info!(
        budget_secs = shutdown_budget.as_secs(),
        "shutting down wamn-host"
    );
    // Admission closes completely before the queue drains.
    let ingress_result = wamn_engine::lifecycle::bounded_cleanup(remaining(), stop_ingress).await;
    let drain_then_cleanup = async {
        wamn_engine::lifecycle::bounded_cleanup(remaining(), native_cleanup).await
    };
    let (native_result, ()) = tokio::join!(
        drain_then_cleanup,
        tokio::time::sleep(propagation_delay.min(remaining()))
    );
    let native_result =
        normalize_explicit_ingress_stop(native_result, had_ingress && ingress_result.is_ok());
    let cleanup_result = ingress_result.and(native_result);
    if let Err(error) = &cleanup_result {
        tracing::error!(%error, "combined host cleanup failed; durable queue remains fenced for recovery");
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

    /// Parse `arguments` with the required drain bound after the program name.
    fn parse<I, T>(arguments: I) -> Result<TestCli, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString>,
    {
        let mut arguments: Vec<std::ffi::OsString> =
            arguments.into_iter().map(Into::into).collect();
        arguments.insert(1.min(arguments.len()), "--drain-bound-seconds=300".into());
        TestCli::try_parse_from(arguments)
    }

    #[test]
    fn the_chart_binding_inherits_the_data_connection_and_keeps_its_grants() {
        use plugin::HostPlugin as _;

        let args = parse([
            "wamn-host",
            "--data-nats-url=nats://wamn-nats.platform.svc.cluster.local:4222",
            "--data-nats-tls-ca=/data-cert/ca.crt",
            "--data-nats-tls-cert=/data-cert/tls.crt",
            "--data-nats-tls-key=/data-cert/tls.key",
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
        assert_eq!(events.get("tls-cert"), Some("/data-cert/tls.crt"));
        assert_eq!(
            events.get("stream-allow"),
            Some("EVT_7_fixture_4_prod_7_widgets")
        );
        assert!(resolve("foreign", &wash_runtime::wit::Config::new()).is_err());
        // R6: workloadConfig deny refuses a workload's connection or wider grant.
        for (key, value) in [
            ("servers", "nats://foreign.invalid:4222"),
            ("tls-cert", "/foreign/tls.crt"),
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
    fn chart_lifecycle_flags_parse_with_native_defaults() {
        let defaults = parse(["wamn-host"]).unwrap().args;
        assert!(defaults.drain_delay.is_zero());
        assert_eq!(defaults.drain_bound_seconds, 300);
        TestCli::try_parse_from(["wamn-host"])
            .expect_err("a host without its drain bound refuses to start");
        assert!(defaults.nats_connect_timeout.is_zero());
        let args = parse([
            "wamn-host",
            "--drain-delay=5s",
            "--nats-connect-timeout=60s",
            "--probe-addr=0.0.0.0:8081",
            "--max-http-ingress-connections=64",
            "--max-concurrent-starts=2",
        ])
        .unwrap()
        .args;
        assert_eq!(args.drain_delay, Duration::from_secs(5));
        assert_eq!(args.nats_connect_timeout, Duration::from_secs(60));
        assert_eq!(args.probe_addr.unwrap().port(), 8081);
        assert_eq!(args.max_http_ingress_connections, Some(64));
        assert_eq!(args.max_concurrent_starts, Some(2));
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
