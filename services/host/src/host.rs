//! The `host` subcommand: a ClusterHost deployable by the runtime-operator
//! Helm chart. Arg surface mirrors what the chart's runtime deployment
//! template renders for `wash host` (charts/runtime-operator).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use clap::Args;
use opentelemetry::global;
use tokio_postgres::NoTls;
use wash_runtime::engine::WasmProposal;
use wash_runtime::engine::host_memory::HostMemoryBudgets;
use wash_runtime::host::HostConfig;
use wash_runtime::host::http::{ConnectionLimit, Ingress};
use wash_runtime::host::probes::{self, Liveness, ProbeState};
use wash_runtime::observability::{MeterKind, Meters};
use wash_runtime::plugin;
use wash_runtime::washlet::{ClusterHostBuilder, NatsConnectionOptions, connect_nats};

use wamn_control_provision::session_target::session_audience;
use wamn_control_provision::{SystemReader, parse_system_reader_url, project_env_database_name};
use wamn_control_registry::Triple;
use wamn_execution_host::{
    ROUTER_DELIVERY_ID, RouterDeliveryBridge, RouterDriver, RouterDriverConfig,
    WIRING_CACHE_CAPACITY_ENV, WiringCacheCapacity,
};
use wamn_platform_identity::route_caller_subject;
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::engine::{
    DEFAULT_CORE_INSTANCES, build_engine_with_host_memory,
    build_engine_with_host_memory_and_compilation_cache,
};
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::flow_http_routing::{
    FlowHttpRouting, RouteAuthentication, SessionRouteAuthentication,
    requires_pat_route_authentication, requires_session_route_authentication,
};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_postgres::AuthorityClass;
use wamn_runtime::plugins::{ClassCredentials, WamnJetstream, WamnLogging, WamnPostgres};
use wamn_runtime::release_manifest::LoadedRelease;
use wamn_runtime::release_manifest_source::ReleaseManifestSource;
use wamn_runtime::session_keys::{IssuerKeys, IssuerKeysConfig};
use wamn_runtime::session_verifier::SessionVerifier;

#[derive(Debug, Args)]
pub struct HostArgs {
    /// The host group label to assign to the host
    #[arg(long = "host-group", default_value = "default")]
    pub host_group: String,

    /// NATS URL for control-plane communications
    #[arg(long = "scheduler-nats-url", default_value = "nats://localhost:4222")]
    pub scheduler_nats_url: String,

    /// How long to retry the initial scheduler connection; zero fails immediately.
    #[arg(long, default_value = "0s", value_parser = humantime::parse_duration)]
    pub nats_connect_timeout: Duration,

    /// Keep serving after draining readiness so traffic removal can propagate.
    #[arg(long, env = "WASH_DRAIN_DELAY", default_value = "0s", value_parser = humantime::parse_duration)]
    pub drain_delay: Duration,

    #[arg(long = "scheduler-nats-tls-ca")]
    pub scheduler_nats_tls_ca: Option<PathBuf>,

    #[arg(long = "scheduler-nats-tls-first", default_value_t = false)]
    pub scheduler_nats_tls_first: bool,

    #[arg(long = "scheduler-nats-tls-cert")]
    pub scheduler_nats_tls_cert: Option<PathBuf>,

    #[arg(long = "scheduler-nats-tls-key")]
    pub scheduler_nats_tls_key: Option<PathBuf>,

    /// NATS URL for data-plane communications. Accepted for chart
    /// compatibility; the DATA-plane plugin (`wamn:jetstream`, l5i9.17) is
    /// instead configured by `WAMN_EVT_NATS_URL` (the event plane's own env
    /// contract, deploy/infra Service `evt-nats`) — this flag's chart default
    /// points at the control plane, which is the wrong NATS for JetStream.
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

    /// Host-private JSON configuration for the platform materializer's native events binding.
    #[arg(long, env = "WAMN_MAT_NATS_BINDING_FILE")]
    pub materializer_nats_binding_file: Option<PathBuf>,

    /// The host name to assign to the host (chart passes the pod IP)
    #[arg(long = "host-name")]
    pub host_name: Option<String>,

    /// Stable node-acquisition owner. Never derived from `--host-name`, whose
    /// chart value is a pod IP and is not a valid runner identity.
    #[arg(long, env = "WAMN_RUNNER")]
    pub runner: Option<String>,

    /// Environment advertised in heartbeats (chart passes the pod namespace)
    #[arg(long = "environment", env = "WASMCLOUD_HOST_ENVIRONMENT")]
    pub environment: Option<String>,

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
    /// HTTP, so credentials travel in the clear and no certificate is checked.
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

    /// Total guest-memory budget reported by the host.
    ///
    /// The cgroup is the aggregate enforcement boundary; wash-runtime uses
    /// this value for budget diagnostics.
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

    /// Guest memory accounting mode. Production uses the native count mode.
    #[arg(long, env = "WASH_GUEST_MEMORY_MODE", default_value = "count", value_parser = ["count"])]
    pub guest_memory_mode: String,

    /// Registry/repository holding this environment's release-manifest
    /// artifacts, paired with [`HostArgs::release_manifest_digest`].
    ///
    /// Both absent means this host serves no release; both present and the pull
    /// unusable means it refuses to start. See [`load_release`] for why that
    /// distinction lives in these arguments' shape rather than in an error.
    ///
    /// The pair is mutually `requires`d, so the third state a pair invents —
    /// base without digest — is refused by clap at startup rather than
    /// discovered as a pod that quietly serves nothing.
    ///
    /// Unlike every flag above it, this one is NOT part of the chart's rendered
    /// surface: `wash host` upstream has no release model. It is carried per
    /// host group by `hostGroups[].extraArgs`.
    #[arg(
        long = "release-artifact-base",
        env = "WAMN_RELEASE_ARTIFACT_BASE",
        requires = "release_manifest_digest"
    )]
    pub release_artifact_base: Option<String>,

    /// SHA-256 digest, `sha256:<hex>`, of the one serving manifest this host is
    /// bound to. Travels in the pod template; the registry's bytes are refused
    /// unless they hash to exactly this.
    #[arg(
        long = "release-manifest-digest",
        env = "WAMN_RELEASE_MANIFEST_DIGEST",
        requires = "release_artifact_base"
    )]
    pub release_manifest_digest: Option<String>,

    /// Maximum resolved wirings retained by the one production router driver.
    #[arg(
        long = "wiring-cache-capacity",
        env = WIRING_CACHE_CAPACITY_ENV,
        default_value_t = WiringCacheCapacity::default()
    )]
    pub wiring_cache_capacity: WiringCacheCapacity,

    /// Explicit registry/repository holding digest-addressed node components.
    #[arg(long, env = "WAMN_COMPONENT_ARTIFACT_BASE")]
    pub component_artifact_base: Option<String>,

    /// Projected `.dockerconfigjson` file for the component registry.
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: Option<PathBuf>,

    /// Mounted production credential-vault file for node capabilities.
    #[arg(long, env = "WAMN_CREDENTIALS_FILE")]
    pub credentials_file: Option<PathBuf>,

    /// Project whose platform pool and component credentials this driver uses.
    #[arg(long, env = "WAMN_PROJECT", default_value = "default")]
    pub project: String,

    /// Organization owning this host's project.
    ///
    /// Required only when the loaded release carries a PAT-protected HTTP
    /// route; it scopes both the route-caller subject and identity-reader URL.
    #[arg(long, env = "WAMN_ORG")]
    pub org: Option<String>,

    /// Trusted HTTPS issuer for session routes, never discovered from a token.
    #[arg(long, env = "WAMN_SESSION_ISSUER", requires_all = ["session_jwks_ca", "session_instance_suffix"])]
    pub session_issuer: Option<String>,

    /// Public CA bundle for the configured identity service.
    #[arg(long, env = "WAMN_SESSION_JWKS_CA", requires = "session_issuer")]
    pub session_jwks_ca: Option<PathBuf>,

    /// Provisioned database instance suffix for the exact session audience.
    #[arg(
        long,
        env = "WAMN_SESSION_INSTANCE_SUFFIX",
        requires = "session_issuer"
    )]
    pub session_instance_suffix: Option<String>,

    /// Optional database search path installed at node checkout.
    #[arg(long, env = "WAMN_SCHEMA")]
    pub schema: Option<String>,

    /// Production outbound ceiling for connection-backed HTTP effects.
    #[arg(long, env = "WAMN_ALLOWED_HOSTS", value_delimiter = ',')]
    pub allowed_hosts: Vec<String>,
}

struct SupervisedIdentityConnection {
    task: tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>,
}

impl SupervisedIdentityConnection {
    fn new(task: tokio::task::JoinHandle<Result<(), tokio_postgres::Error>>) -> Self {
        Self { task }
    }

    async fn failure(&mut self) -> anyhow::Error {
        match (&mut self.task).await {
            Ok(Ok(())) => anyhow::anyhow!("system identity database connection ended"),
            Ok(Err(error)) => {
                anyhow::Error::new(error).context("system identity database connection failed")
            }
            Err(error) => {
                anyhow::Error::new(error).context("system identity database connection task failed")
            }
        }
    }

    async fn shutdown(&mut self) {
        if !self.task.is_finished() {
            self.task.abort();
            let _ = (&mut self.task).await;
        }
    }
}

impl Drop for SupervisedIdentityConnection {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Pull and verify this process's release, or record that it carries none.
///
/// This is the wash host's release load site: the one place in this process
/// that turns registry bytes into a manifest. flow-http routing
/// (`wamn-0h0g.15.96`) and jetstream delivery gating (`wamn-0h0g.15.95`) take the
/// loaded manifest from here by reference; nobody loads a second copy.
///
/// # Why the registry and not a mount
///
/// A projected ConfigMap carries no usable binding between the bytes and the
/// name the template asked for: the same template places both, so comparing
/// them tests the template against itself. A registry is a third party — the
/// digest travels in the pod template, the bytes come from the registry, and the
/// pull refuses unless they agree (`wamn-0h0g.15.98`).
///
/// # The absent-release posture
///
/// The two absent cases are told apart by the *arguments*, never by inspecting a
/// failure:
///
/// - **No pair passed.** This host was never given a release: nothing to pull
///   and nothing to refuse. It serves exactly as it did before the release model
///   existed.
/// - **Passed, but the artifact is absent, unreachable or non-canonical.** This
///   host was told it serves a release and cannot. Startup fails and the pod
///   never goes ready — the only refusal worth making, since a host serving an
///   unverified manifest would be routing and gating against nothing.
///
/// The half-passed third state cannot reach here at all: the two arguments
/// mutually `requires` each other, so clap refuses it at startup.
///
/// The node-component registry is deliberately a separate explicit argument.
/// A release artifact establishes release identity; it never implies where
/// digest-addressed component bytes may be pulled from.
async fn load_release(
    artifact_base: Option<&str>,
    manifest_digest: Option<&str>,
    insecure_registry: bool,
    registry_auth_file: Option<&Path>,
    ca_paths: &[PathBuf],
) -> anyhow::Result<Option<Arc<LoadedRelease>>> {
    let (Some(artifact_base), Some(manifest_digest)) = (artifact_base, manifest_digest) else {
        return Ok(None);
    };
    let registry_auth_file =
        registry_auth_file.context("a release-backed host requires --registry-auth-file")?;
    let source = ReleaseManifestSource::new(artifact_base, insecure_registry, registry_auth_file)
        .context("configure the release-manifest registry")?
        .with_ca_paths(ca_paths)
        .context("trust the configured OCI CA bundles for the release pull")?;
    let canonical_bytes = source
        .pull_verified(manifest_digest)
        .await
        .context("pull the serving release manifest")?;
    let origin = format!("{artifact_base}@{manifest_digest}");
    let loaded_release =
        LoadedRelease::load_canonical_bytes(&canonical_bytes, &origin).map_err(|error| {
            anyhow::anyhow!(
                "serving release manifest {origin} is unusable ({:?}): {error}",
                error.kind()
            )
        })?;
    // Shared by reference-count rather than by borrow: every release-gated
    // plugin and the router driver are `Arc`-owned, so none can hold a lifetime
    // tied to `run`'s stack. One allocation remains the process's only manifest.
    Ok(Some(Arc::new(loaded_release)))
}

/// Declare the platform binding through the native host configuration policy.
fn materializer_nats_bindings(config: HashMap<String, String>) -> plugin::PluginBindings {
    plugin::PluginBindings::new().with_plugin(
        plugin::PluginBindingSet::new(plugin::wasmcloud_nats::PLUGIN_NATS_ID)
            .with_binding("events", config)
            .with_workload_config(plugin::WorkloadConfigPolicy::Deny),
    )
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
        Some(args.core_instances),
    )
    .map_err(anyhow::Error::msg)
}

/// Route each sourced url onto the AUTHORITY CLASS it belongs to
/// (`wamn-0h0g.22.16`, `wamn-0h0g.22.31`, `wamn-0h0g.22.11`).
///
/// A FUNCTION RATHER THAN A CLOSURE IN `run` BECAUSE THE ROUTING IS THE SECURITY
/// PROPERTY AND `run` IS UNREACHABLE FROM A TEST — the `executor_credentials`
/// precedent. Inline, a mutant that names the wrong class, or that keeps the
/// shared `every_class` entry on the absent arm, leaves a platform pool on the
/// guest login and SURVIVES every test in this crate, because nothing can
/// observe the composition without standing up a NATS cluster, a release and a
/// database.
///
/// Each cut-over family carries `Option<String>` rather than a required url:
/// unlike the executor, a host serves with no run-plane work and no trusted HTTP
/// effect at all, so an absent generation UNNAMES its class and startup
/// continues. Unnaming is not the same as leaving the shared entry in place —
/// see [`ClassCredentials::without_class`].
fn host_credentials(
    database_url: Option<String>,
    executor_platform_url: Option<String>,
    http_admitter_url: Option<String>,
    event_materializer_url: Option<String>,
) -> ClassCredentials {
    let credentials = database_url.map_or_else(ClassCredentials::default, |url| {
        ClassCredentials::every_class(url)
    });
    let credentials = match executor_platform_url {
        Some(platform) => credentials.with_class(AuthorityClass::ExecutorPlatform, platform),
        None => credentials.without_class(AuthorityClass::ExecutorPlatform),
    };
    let credentials = match http_admitter_url {
        Some(admitter) => credentials.with_class(AuthorityClass::CallableHttp, admitter),
        None => credentials.without_class(AuthorityClass::CallableHttp),
    };
    match event_materializer_url {
        Some(materializer) => {
            credentials.with_class(AuthorityClass::EventMaterializer, materializer)
        }
        None => credentials.without_class(AuthorityClass::EventMaterializer),
    }
}

/// Name the callable-HTTP credential only when the loaded release demands PAT
/// authorization. Configuration is transport, not authority: a release-less or
/// anonymous-only host must ignore an ambient URL rather than acquire its pool.
fn demanded_http_admitter_url(pat_routes: bool, configured_url: Option<String>) -> Option<String> {
    if pat_routes { configured_url } else { None }
}

fn session_verifier(
    args: &HostArgs,
    environment: &str,
    http_admitter_url: &str,
) -> anyhow::Result<SessionVerifier> {
    let org = args
        .org
        .as_deref()
        .context("a session route requires --org/WAMN_ORG")?;
    let issuer = args
        .session_issuer
        .as_deref()
        .context("a session route requires --session-issuer")?;
    let suffix = args
        .session_instance_suffix
        .as_deref()
        .context("a session route requires --session-instance-suffix")?;
    let audience = session_audience(&Triple::new(org, &args.project, environment), suffix)?;
    let database = project_env_database_name(org, &args.project, environment, suffix);
    // Compare trusted deployment coordinates, not anything carried by a token.
    // Parsing failures never attach the credential-bearing URL to diagnostics.
    let connection = http_admitter_url
        .parse::<tokio_postgres::Config>()
        .map_err(|_| anyhow::anyhow!("session permission reader URL is invalid"))?;
    anyhow::ensure!(
        connection.get_dbname() == Some(database.as_str()),
        "session audience and permission database differ"
    );
    let ca_path = args
        .session_jwks_ca
        .as_deref()
        .context("a session route requires --session-jwks-ca")?;
    let ca = std::fs::read(ca_path).context("read the session issuer public CA bundle")?;
    let mut endpoint = url::Url::parse(issuer).context("parse the configured session issuer")?;
    endpoint.set_path("/.well-known/jwks.json");
    endpoint.set_query(None);
    let keys = IssuerKeys::new(IssuerKeysConfig::new(issuer, endpoint.as_str(), &ca)?)?;
    SessionVerifier::new(keys, org, &audience).context("configure the host session verifier")
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

    // THE RELEASE LOADS FIRST, and the ordering is load-bearing rather than
    // tidy. Under ruling wamn-0h0g.15.102 the verified manifest is the SOLE carrier
    // of the (effective release id, manifest digest) pair, so every consumer takes the
    // pair from this object — including the claim-time recording that
    // wamn-0h0g.15.103 repoints at it. A component that bound before the loaded release
    // existed would have no pair to record. Building it here, ahead of the NATS
    // connections, the engine and every plugin, makes that ordering impossible to
    // get wrong, and makes a host that cannot verify its release refuse before it
    // opens a socket. The one-site-per-process guard in
    // tests/conformance/src/runtime_policy.rs pins both facts across both hosts.
    //
    // It also runs after the CA install above, which is why the release pull can
    // reach a registry behind the chart's own CA.
    let release = load_release(
        args.release_artifact_base.as_deref(),
        args.release_manifest_digest.as_deref(),
        args.allow_insecure_registries,
        args.registry_auth_file.as_deref(),
        &args.oci_ca_paths,
    )
    .await?;
    let pat_routes = release
        .as_ref()
        .is_some_and(|loaded_release| requires_pat_route_authentication(loaded_release.manifest()));
    let session_routes = release
        .as_ref()
        .is_some_and(|loaded_release| requires_session_route_authentication(loaded_release.manifest()));
    let http_admitter_url = std::env::var("WAMN_HTTP_ADMITTER_PG_URL")
        .ok()
        .filter(|url| !url.is_empty());
    let session_verifier = if session_routes {
        let loaded_release = release
            .as_ref()
            .expect("a session route belongs to a loaded release");
        Some(session_verifier(
            &args,
            &loaded_release.manifest().release.environment,
            http_admitter_url
                .as_deref()
                .context("a session route requires WAMN_HTTP_ADMITTER_PG_URL")?,
        )?)
    } else {
        None
    };

    // The release pull above is what makes the PAT requirement knowable. Once
    // it is known, settle every scoped input before opening the identity, NATS,
    // project-database, or ingress sockets. A release-less or anonymous-only
    // host takes the absent arm and acquires no identity-reader connection.
    let (route_auth_scope, identity_reader, mut identity_connection) = if pat_routes {
        let loaded_release = release
            .as_ref()
            .expect("a PAT route was found only inside a loaded release");
        let org = args
            .org
            .as_deref()
            .filter(|org| !org.is_empty())
            .context("a PAT-protected route requires --org/WAMN_ORG")?;
        let project = (!args.project.is_empty())
            .then_some(args.project.as_str())
            .context("a PAT-protected route requires a nonempty --project/WAMN_PROJECT")?;
        let system_url = std::env::var("WAMN_SYSTEM_URL")
            .ok()
            .filter(|url| !url.is_empty())
            .context("a PAT-protected route requires WAMN_SYSTEM_URL")?;
        http_admitter_url
            .as_deref()
            .context("a PAT-protected route requires WAMN_HTTP_ADMITTER_PG_URL")?;
        let subject = route_caller_subject(org, project, &loaded_release.manifest().release.environment)
            .context("derive the scoped route-caller subject")?;
        parse_system_reader_url(
            SystemReader::Identity,
            &system_url,
            org,
            project,
            &loaded_release.manifest().release.environment,
        )?;
        let (client, connection) = tokio_postgres::connect(&system_url, NoTls)
            .await
            .context("connect the scoped system identity reader")?;
        (
            Some((org.to_owned(), subject)),
            Some(Arc::new(client)),
            Some(SupervisedIdentityConnection::new(tokio::spawn(connection))),
        )
    } else {
        (None, None, None)
    };
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

    let scheduler_nats_client = connect_nats(
        args.scheduler_nats_url.clone(),
        NatsConnectionOptions {
            connect_retry: (!args.nats_connect_timeout.is_zero())
                .then_some(args.nats_connect_timeout),
            request_timeout: None,
            tls_ca: args.scheduler_nats_tls_ca.clone(),
            tls_first: args.scheduler_nats_tls_first,
            tls_cert: args.scheduler_nats_tls_cert.clone(),
            tls_key: args.scheduler_nats_tls_key.clone(),
        },
    )
    .await
    .context("failed to connect to scheduler NATS")?;
    // l5i9.17: the wamn:jetstream doorbell rides the SAME control-plane
    // connection (the dispatcher publishes and the run-worker subscribes on the
    // shared execution-target doorbell subject) — no second connection.
    let doorbell_client = scheduler_nats_client.clone();

    let host_memory = host_memory(&args)?;
    let engine = Arc::new(
        if let Some(compilation_cache_dir) = args.wasmtime_cache_dir.as_deref() {
            build_engine_with_host_memory_and_compilation_cache(
                &args.wasm_proposals,
                host_memory,
                compilation_cache_dir,
            )?
        } else {
            build_engine_with_host_memory(&args.wasm_proposals, host_memory)?
        },
    );
    // wamn-0h0g.22.8.3: the host NAMES its own credential sources here rather
    // than letting the config layer pick one up implicitly. deploy/platform
    // injects WAMN_PG_URL via secretKeyRef (values-host-default.yaml ->
    // host-db.example.yaml), so the environment is the transport; reading it at
    // composition is what makes it the explicit source instead of a fallback.
    // A PAT-only host without guest SQL can instead name just CallableHttp below;
    // no base URL is borrowed for that authority.
    // wamn-0h0g.22.16: the host also names WHICH AUTHORITY the credential
    // belongs to.
    //
    // wamn-0h0g.22.31 CUT THE EXECUTOR-PLATFORM CLASS OVER, and the ABSENT arm
    // is the load-bearing half. Once that family authenticates as its own
    // provisioned generation, the shared `WAMN_PG_URL` login stops being a
    // placeholder for it and becomes a GUEST credential that would still satisfy
    // the map — and `pool::credential_exactness_hook` refuses it on every
    // physical connection anyway, for failing
    // `pg_has_role(current_user, 'wamn_executor_platform', MEMBER)`. So a host
    // given no executor-platform generation UNNAMES the class rather than
    // keeping the guest entry: checkout then refuses for a missing credential,
    // which is the real fault, instead of for a membership the guest login can
    // never hold. The host is NOT refused at startup for this — unlike the
    // executor it can serve with no run-plane work at all.
    //
    // wamn-0h0g.22.11 CUT THE CALLABLE-HTTP CLASS OVER on the same terms.
    // Trusted component HTTP effects remain executor-only (wamn-0h0g.10.16),
    // but wamn-10yt.3.2 reuses that exact pool family for the host-owned
    // app_system.permissions read a PAT route requires. The manifest-derived
    // gate above makes the generation mandatory for that demand only; an
    // anonymous-only or release-less host retains the absent, fail-closed arm.
    // EventMaterializer uses the same explicit-source rule: only the dedicated
    // URL names that class, and absence removes the shared guest placeholder.
    let executor_platform_url = std::env::var("WAMN_EXECUTOR_PLATFORM_PG_URL")
        .ok()
        .filter(|url| !url.is_empty());
    let event_materializer_url = std::env::var("WAMN_EVENT_MATERIALIZER_PG_URL")
        .ok()
        .filter(|url| !url.is_empty());
    let http_admitter_url =
        demanded_http_admitter_url(pat_routes || session_routes, http_admitter_url);
    let guest_url = std::env::var("WAMN_PG_URL")
        .ok()
        .filter(|url| !url.is_empty());
    let postgres_credentials = if guest_url.is_some()
        || executor_platform_url.is_some()
        || http_admitter_url.is_some()
        || event_materializer_url.is_some()
    {
        Some(host_credentials(
            guest_url,
            executor_platform_url,
            http_admitter_url,
            event_materializer_url,
        ))
    } else {
        None
    };
    let postgres = Arc::new(
        WamnPostgres::from_env_for_project(&args.project, postgres_credentials)
            .context("wamn:postgres plugin init")?,
    );
    let logging = Arc::new(WamnLogging::from_env().context("wamn:logging plugin init")?);
    let http_transport = Arc::new(HttpTransport::new().context("HTTP transport init")?);
    let router_driver = match release.as_ref() {
        Some(release) => {
            let artifact_base = args
                .component_artifact_base
                .as_deref()
                .context("a serving host requires --component-artifact-base")?;
            let registry_auth_file = args
                .registry_auth_file
                .as_deref()
                .context("a serving host requires --registry-auth-file")?;
            let source_config = ComponentArtifactSourceConfig::new(
                artifact_base,
                args.allow_insecure_registries,
                Duration::from_secs(30),
            )?
            .with_registry_auth_file(registry_auth_file)
            .context("load component registry pull credential")?
            .with_ca_paths(&args.oci_ca_paths)
            .context("trust the configured OCI CA bundles for component pulls")?;
            let source = ComponentArtifactSource::new(source_config);
            let credentials = Arc::new(match &args.credentials_file {
                Some(path) => WamnCredentials::from_file(path)?,
                None => WamnCredentials::empty(),
            });
            let allowed_hosts = args
                .allowed_hosts
                .iter()
                .map(|value| value.parse())
                .collect::<Result<Vec<_>, _>>()?;
            Some(Arc::new(RouterDriver::new(
                Arc::clone(&engine),
                Arc::clone(&postgres),
                Arc::clone(&http_transport),
                credentials,
                Arc::clone(&logging),
                allowed_hosts.into(),
                Arc::clone(release),
                source,
                RouterDriverConfig {
                    owner_prefix: router_owner,
                    project: args.project.clone(),
                    schema: args.schema.clone(),
                    cache_capacity: args.wiring_cache_capacity,
                },
            )?))
        }
        None => None,
    };
    // SCHEDULE-TIME PRELOAD. prepare_synchronous_release pulls and compiles
    // every digest this release serves and populates the driver's digest-keyed
    // component cache; without it the FIRST request pays a full pull and compile
    // -- measured at 35.8 s on this journey (evidence/perf/2026.09/1-component-cache.md).
    //
    // It ran only in services/executor before this: the probe was constructed
    // there and nowhere else, so the host that actually serves HTTP routes never
    // preloaded anything. Readiness is gated on the same call, so a host that
    // reports ready has the entries rather than a promise of them.
    if let Some(driver) = router_driver.as_ref() {
        let readiness = wamn_execution_host::RouterReadinessProbe::new(Arc::clone(driver));
        let snapshot = readiness.refresh().await;
        anyhow::ensure!(
            matches!(
                snapshot.status,
                wamn_execution_host::RouterReadinessStatus::Ready
            ),
            "release preload did not reach ready: {:?} ({})",
            snapshot.status,
            snapshot.refusal.unwrap_or("no refusal recorded")
        );
        tracing::info!(
            target: "wamn::host",
            "release preload completed; component cache warm"
        );
    }
    let host_config = HostConfig {
        allow_oci_insecure: args.allow_insecure_registries,
        oci_pull_timeout: Some(Duration::from_secs(30)),
        oci_cache_dir: args.oci_cache_dir.clone(),
        oci_ca_paths: args.oci_ca_paths.clone(),
    };
    let jetstream = Arc::new(
        WamnJetstream::from_env()
            .with_doorbell(doorbell_client)
            .with_release(release.clone()),
    );
    jetstream
        .activate_events()
        .await
        .map_err(|error| anyhow::anyhow!("activate declared event streams: {error:?}"))?;
    let flow_http =
        FlowHttpRouting::from_env(release.clone()).context("wamn:flow-http-routing plugin init")?;
    let flow_http = match (&route_auth_scope, &identity_reader) {
        (Some((org, subject)), Some(identity_reader)) => flow_http.with_authentication(Arc::new(
            RouteAuthentication::new(
                Arc::clone(identity_reader),
                Arc::clone(&postgres),
                org.clone(),
                args.project.clone(),
                subject.clone(),
            )
            .await
            .context("prepare the route authentication identity reads")?,
        )),
        (None, None) => flow_http,
        _ => unreachable!("route authentication inputs are constructed together"),
    };
    let flow_http = match session_verifier {
        Some(verifier) => flow_http.with_session_authentication(Arc::new(
            SessionRouteAuthentication::new(verifier, Arc::clone(&postgres), args.project.clone()),
        )),
        None => flow_http,
    };

    let mut plugins: Vec<Arc<dyn plugin::HostPlugin>> = vec![
        Arc::new(
            plugin::wasi_config::DynamicConfig::builder()
                .copy_environment(true)
                .build(),
        ),
        // S5: the custom wamn:logging plugin replaces the native TracingLogger
        // — it enriches (host-trusted tenant/project + guest flow/run/node),
        // owns a bounded front queue + drop counter, and ships enriched OTel log
        // records to the collector. Both claim wasi:logging/logging, so exactly
        // one may be registered.
        logging,
        Arc::new(plugin::wasi_otel::WasiOtel::default()),
        // Pool config from WAMN_PG_URL + the WAMN_PG_* tuning env; without a URL
        // the plugin still links and returns connection-unavailable on use.
        postgres,
        // l5i9.17: the wamn:jetstream plugin (E10), first bound by the
        // Service-first materializer. Data-plane URL from WAMN_EVT_NATS_URL
        // (absent ⇒ links but returns connection-unavailable, the WAMN_PG_*
        // posture); the doorbell rings on the control-plane client above.
        // wamn-0h0g.15.95: READER 4 of the loaded release. Delivery is gated on the serving
        // release's registration projection, so an event whose registration
        // identity is not in this release never reaches a component. The plugin
        // takes the loaded manifest — it does not load one.
        jetstream.clone(),
        // wamn-0h0g.15.96: READER 3 of the loaded release. Route projection stays wholly
        // in-memory; a PAT-protected route additionally carries the scoped
        // identity reader and preloads exact grants from the existing
        // callable-HTTP project pool during authentication.
        Arc::new(flow_http),
    ];

    if let (Some(driver), Some(release)) = (&router_driver, &release) {
        plugins.push(Arc::new(
            RouterDeliveryBridge::new(
                Arc::clone(driver),
                Arc::clone(release),
                Arc::clone(&jetstream),
                &args.project,
            )?
            // The bridge defaults to no meter so a test can own its own provider
            // and read back exactly the series one bridge emitted. Production has
            // no second provider to own, so it takes the process-global one that
            // `initialize_observability` installs — a no-op meter when no OTEL_
            // variable is set. Without this call both `wamn.router.delivery`
            // series exist and stay permanently silent (wamn-1fhk).
            .with_metrics(&global::meter(ROUTER_DELIVERY_ID)),
        ));
    }

    let native_event_bindings = if let Some(path) = &args.materializer_nats_binding_file {
        let bytes = tokio::fs::read(path)
            .await
            .context("read the platform materializer NATS binding")?;
        let config = serde_json::from_slice(&bytes)
            .context("decode the platform materializer NATS binding")?;
        plugins.push(Arc::new(
            plugin::wasmcloud_nats::WasmcloudNats::new()
                .with_memory_budget(host_memory.max_guest_memory)
                .with_lattice_prefixes(vec![
                    format!("{}.", wash_runtime::washlet::HOST_API_PREFIX),
                    format!("{}.", wash_runtime::washlet::OPERATOR_API_PREFIX),
                ]),
        ));
        Some(materializer_nats_bindings(config))
    } else {
        None
    };

    // Count the same plugins supplied to the native host, including the optional bridge.
    let cleanup_budget = host_cleanup_budget(plugins.len())?;
    let mut host_builder = wash_runtime::host::HostBuilder::default();
    for plugin in plugins {
        host_builder = host_builder.with_plugin(plugin)?;
    }
    if let Some(bindings) = native_event_bindings {
        host_builder = host_builder.with_plugin_bindings(bindings);
    }
    let mut builder = ClusterHostBuilder::default()
        .with_host_builder(host_builder)
        .with_engine((*engine).clone())
        .with_host_config(host_config)
        .with_meters(Meters::new(MeterKind::Duration))
        .with_nats_client(Arc::new(scheduler_nats_client))
        .with_host_group(args.host_group.clone());

    if let Some(host_name) = &args.host_name {
        builder = builder.with_host_name(host_name);
    }
    if let Some(environment) = &args.environment {
        builder = builder.with_environment(environment);
    }

    if let Some(starts) = args.max_concurrent_starts {
        builder = builder.with_max_concurrent_starts(starts);
    }
    let silence_budget = wash_runtime::washlet::liveness_silence(builder.heartbeat_interval());
    let liveness = Liveness::new(silence_budget);
    builder = builder.with_liveness(Arc::clone(&liveness));
    let probe_state = ProbeState::default().with_liveness(Arc::clone(&liveness));
    let mut ingress_connections = None;
    if let Some(addr) = args.http_addr {
        let router = wamn_runtime::expected_router::expected_host_router(release.as_deref());
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
        let server = ingress.build().await?;
        let connections = server.connection_limit();
        probe_state.register(Arc::new(connections.clone()));
        ingress_connections = Some(connections);
        builder = builder.with_http_handler(Arc::new(server));
    }
    let probe_listener = match args.probe_addr {
        Some(address) => Some(probes::bind(address).await?),
        None => None,
    };

    let cluster_host = builder.build().context("failed to build cluster host")?;
    tracing::info!(
        router_delivery = router_driver.is_some(),
        "wamn-host starting (base plugins: wasi:config, wamn:logging, wasi:otel, wamn:postgres, wamn:jetstream, wamn:flow-http-routing)"
    );
    // Whether this host carries a release is the first thing an operator needs from
    // the log: it decides whether the release-gated interfaces have a manifest to
    // serve from at all. The binding also has to outlive this point — `release`
    // holds the process's only loaded manifest for as long as `run` is on the
    // stack, which is the whole serving period.
    match release.as_ref() {
        Some(loaded_release) => tracing::info!(
            effective_release_id = loaded_release.release().effective_release_id,
            manifest_digest = %loaded_release.release().manifest_digest,
            "wamn-host loaded its release"
        ),
        None => {
            tracing::info!("wamn-host carries no release; no release-gated interface is served")
        }
    }
    let mut probe_tasks = tokio::task::JoinSet::new();
    if let Some(listener) = probe_listener {
        probe_tasks.spawn(probes::serve(
            listener,
            probe_state.clone(),
            std::future::pending(),
        ));
    }
    let cleanup = wash_runtime::washlet::run_cluster_host(cluster_host)
        .await
        .context("failed to start cluster host")?;
    tracing::info!(
        elapsed_ms = %startup_started.elapsed().as_millis(),
        cleanup_budget_secs = cleanup_budget.as_secs(),
        "wamn-host runtime startup completed"
    );

    // Polling cleanup requests shutdown. Observe the retained native state instead.
    let stopped = async {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let identity_failure = async {
            let Some(connection) = identity_connection.as_mut() else {
                return std::future::pending::<anyhow::Error>().await;
            };
            connection.failure().await
        };
        tokio::select! {
            signal = tokio::signal::ctrl_c() => signal.context("receive SIGINT"),
            _ = sigterm.recv() => Ok(()),
            error = identity_failure => Err(error),
            error = wamn_runtime::lifecycle::watch_liveness(&liveness, &probe_state, silence_budget) => Err(error),
            () = ingress_stopped(ingress_connections.as_ref()) => anyhow::bail!("native HTTP ingress stopped unexpectedly"),
            task = probe_tasks.join_next(), if !probe_tasks.is_empty() => {
                Err(probe_listener_failure(task))
            }
        }
    };
    let result = stop_after(
        stopped,
        cleanup,
        &probe_state,
        args.drain_delay,
        cleanup_budget,
    )
    .await;
    // Abort and join auxiliary tasks even when the native command task skipped Host::stop.
    let identity_result = wamn_runtime::lifecycle::bounded_cleanup(Duration::from_secs(1), async {
        if let Some(mut connection) = identity_connection.take() {
            connection.shutdown().await;
        }
        Ok(())
    })
    .await;
    probe_tasks.abort_all();
    let probe_result = wamn_runtime::lifecycle::bounded_cleanup(Duration::from_secs(1), async {
        while probe_tasks.join_next().await.is_some() {}
        Ok(())
    })
    .await;
    result.and(identity_result).and(probe_result)
}

fn probe_listener_failure(task: Option<Result<(), tokio::task::JoinError>>) -> anyhow::Error {
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

async fn stop_after(
    stopped: impl std::future::Future<Output = anyhow::Result<()>>,
    cleanup: impl std::future::Future<Output = anyhow::Result<()>>,
    probes: &ProbeState,
    drain_delay: Duration,
    cleanup_budget: Duration,
) -> anyhow::Result<()> {
    let result = stopped.await;
    probes.drain();
    if result.is_ok() && !drain_delay.is_zero() {
        tokio::time::sleep(drain_delay).await;
    }
    tracing::info!("shutting down wamn-host");
    let cleanup_result = wamn_runtime::lifecycle::bounded_cleanup(cleanup_budget, cleanup).await;
    if let Err(error) = &cleanup_result {
        // Subscribe failure and command-task panic may bypass native Host::stop.
        tracing::error!(%error, "native host cleanup failed; plugin cleanup may be incomplete");
    }
    result.and(cleanup_result)
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    const RELEASE_BASE: &str = "registry.invalid/wamn/releases";
    const RELEASE_DIGEST: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    #[derive(Debug, clap::Parser)]
    struct TestCli {
        #[command(flatten)]
        args: HostArgs,
    }

    #[test]
    fn materializer_binding_keeps_credentials_and_grants_host_owned() {
        let config: HashMap<String, String> = [
            ("servers", "nats://event-broker.invalid:4222"),
            ("username", "materializer"),
            ("password", "test-password"),
            ("inbox-prefix", "_INBOX_materializer"),
            ("stream-allow", "EVT_4_acme_9_receiving_4_prod"),
            ("subject-allow", "evt.acme.receiving.prod.>"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        let bindings = materializer_nats_bindings(config.clone());
        let declared = bindings.for_plugin(plugin::wasmcloud_nats::PLUGIN_NATS_ID);
        let schema = plugin::wasmcloud_nats::binding_schema();
        let native = plugin::wasmcloud_nats::WasmcloudNats::new();
        use plugin::HostPlugin as _;
        native.validate_bindings(&declared).unwrap();
        let narrows = |key: &str, ceiling: &str, value: &str| native.narrows(key, ceiling, value);
        let resolve = |name: &str, overrides: &HashMap<String, String>| {
            declared.resolve(name, overrides, &schema, &narrows)
        };
        assert_eq!(resolve("events", &HashMap::new()).unwrap(), config);
        assert!(resolve("", &HashMap::new()).unwrap().is_empty());
        assert!(resolve("foreign", &HashMap::new()).is_err());
        for (key, value) in [
            ("servers", "nats://foreign.invalid:4222"),
            ("username", "administrator"),
            ("password", "foreign-password"),
            ("inbox-prefix", "_INBOX_foreign"),
            ("stream-allow", ">"),
            ("subject-allow", ">"),
        ] {
            let overrides = HashMap::from([(key.to_owned(), value.to_owned())]);
            let error = resolve("events", &overrides).unwrap_err().to_string();
            assert!(error.contains(key), "{error}");
            assert!(!error.contains("test-password"), "{error}");
            assert!(!error.contains("foreign-password"), "{error}");
        }
    }

    #[test]
    fn chart_lifecycle_flags_parse_with_native_defaults() {
        let defaults = TestCli::try_parse_from(["wamn-host"]).unwrap().args;
        assert!(defaults.drain_delay.is_zero());
        assert!(defaults.nats_connect_timeout.is_zero());
        let args = TestCli::try_parse_from([
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

    #[tokio::test]
    async fn cleanup_is_not_polled_before_the_shutdown_trigger() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let cleanup_polled = AtomicBool::new(false);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let probes = ProbeState::default();
        let running = stop_after(
            async { stopped.await.context("test shutdown sender") },
            async {
                cleanup_polled.store(true, Ordering::Relaxed);
                Ok(())
            },
            &probes,
            Duration::ZERO,
            Duration::from_secs(1),
        );
        tokio::pin!(running);
        tokio::select! {
            biased;
            _ = &mut running => panic!("service must wait for shutdown"),
            () = tokio::task::yield_now() => {}
        }
        assert!(!cleanup_polled.load(Ordering::Relaxed));
        stop.send(()).unwrap();
        running.await.unwrap();
        assert!(cleanup_polled.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn native_probe_termination_triggers_failure_and_cleanup() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        tokio::time::timeout(Duration::from_secs(5), async {
            let listener = probes::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
            let address = listener.local_addr().unwrap();
            let state = ProbeState::default();
            state.started();
            let mut tasks = tokio::task::JoinSet::new();
            let probe = tasks.spawn(probes::serve(
                listener,
                state.clone(),
                std::future::pending(),
            ));
            let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
            client
                .write_all(b"GET /livez HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
            let mut response = String::new();
            client.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with("HTTP/1.1 200"), "{response}");

            let cleanup_polled = AtomicBool::new(false);
            let running = stop_after(
                async {
                    let task = tasks.join_next().await;
                    assert!(task.as_ref().unwrap().as_ref().unwrap_err().is_cancelled());
                    Err(probe_listener_failure(task))
                },
                async {
                    cleanup_polled.store(true, Ordering::Relaxed);
                    Ok(())
                },
                &state,
                Duration::from_secs(60),
                Duration::from_secs(1),
            );
            tokio::pin!(running);
            tokio::select! {
                biased;
                _ = &mut running => panic!("the live listener must not trigger cleanup"),
                () = tokio::task::yield_now() => {}
            }
            assert!(!cleanup_polled.load(Ordering::Relaxed));
            probe.abort();
            let error = running.await.unwrap_err();
            assert!(
                error
                    .to_string()
                    .starts_with("native probe listener stopped unexpectedly:")
            );
            assert!(cleanup_polled.load(Ordering::Relaxed));
        })
        .await
        .expect("native listener failure must trigger cleanup without the traffic drain delay");
    }

    #[tokio::test]
    async fn failure_skips_traffic_delay_and_cannot_hide_incomplete_cleanup() {
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            stop_after(
                async { anyhow::bail!("native ingress stopped") },
                std::future::pending(),
                &ProbeState::default(),
                Duration::from_secs(60),
                Duration::ZERO,
            ),
        )
        .await
        .expect("failure must skip the signal-only drain delay");
        assert_eq!(result.unwrap_err().to_string(), "native ingress stopped");
        let result = stop_after(
            async { Ok(()) },
            async { anyhow::bail!("native cleanup failed") },
            &ProbeState::default(),
            Duration::ZERO,
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(result.unwrap_err().to_string(), "native cleanup failed");
    }

    const GUEST: &str = "postgres://guest@h/db";
    const PLATFORM: &str = "postgres://platform@h/db";
    const ADMITTER: &str = "postgres://admitter@h/db";
    const MATERIALIZER: &str = "postgres://materializer@h/db";

    /// THE GUEST URL IS NOT A CUT-OVER FAMILY'S CREDENTIAL (`wamn-0h0g.22.31`,
    /// `wamn-0h0g.22.11`).
    ///
    /// Driven through [`host_credentials`], the same call `run` makes, so the
    /// ROUTING is what is pinned rather than a re-statement of it. Every class is
    /// checked, not just the ones that matter: naming the wrong class moves BOTH
    /// the class that gained a cut-over url and the one that lost it, and a
    /// two-class assertion could miss the second half.
    #[test]
    fn the_host_routes_each_url_to_its_own_class() {
        let credentials = host_credentials(
            Some(GUEST.to_owned()),
            Some(PLATFORM.to_owned()),
            Some(ADMITTER.to_owned()),
            Some(MATERIALIZER.to_owned()),
        );
        for class in AuthorityClass::ALL {
            let expected = match class {
                AuthorityClass::ExecutorPlatform => PLATFORM,
                AuthorityClass::CallableHttp => ADMITTER,
                AuthorityClass::EventMaterializer => MATERIALIZER,
                _ => GUEST,
            };
            assert_eq!(
                credentials.url(class),
                Some(expected),
                "{class:?} must authenticate with its own credential"
            );
        }
    }

    /// A HOST GIVEN NO GENERATION FOR A CUT-OVER FAMILY UNNAMES THAT CLASS
    /// (`wamn-0h0g.22.31`, `wamn-0h0g.22.11`).
    ///
    /// The load-bearing half. Keeping `every_class`'s shared entry would leave
    /// the family authenticating as the GUEST — a login of another authority
    /// that still satisfies the map — so the absent arm must ERASE the entry and
    /// let checkout refuse for the missing credential. Each arm is exercised on
    /// its own so a mutant that unnames both, or the wrong one, fails here.
    #[test]
    fn a_host_with_no_generation_for_a_family_refuses_rather_than_borrows() {
        let no_admitter = host_credentials(
            Some(GUEST.to_owned()),
            Some(PLATFORM.to_owned()),
            None,
            Some(MATERIALIZER.to_owned()),
        );
        assert_eq!(
            no_admitter.url(AuthorityClass::CallableHttp),
            None,
            "an unprovisioned callable-HTTP family must have NO credential rather \
             than the guest's"
        );
        assert_eq!(
            no_admitter.url(AuthorityClass::ExecutorPlatform),
            Some(PLATFORM),
            "unnaming callable-HTTP must not disturb another class"
        );

        let no_platform = host_credentials(
            Some(GUEST.to_owned()),
            None,
            Some(ADMITTER.to_owned()),
            Some(MATERIALIZER.to_owned()),
        );
        assert_eq!(
            no_platform.url(AuthorityClass::ExecutorPlatform),
            None,
            "an unprovisioned executor-platform family must have NO credential \
             rather than the guest's"
        );
        assert_eq!(
            no_platform.url(AuthorityClass::CallableHttp),
            Some(ADMITTER),
            "unnaming executor-platform must not disturb another class"
        );

        let no_materializer = host_credentials(
            Some(GUEST.to_owned()),
            Some(PLATFORM.to_owned()),
            Some(ADMITTER.to_owned()),
            None,
        );
        assert_eq!(
            no_materializer.url(AuthorityClass::EventMaterializer),
            None,
            "an unprovisioned materializer must not borrow the guest credential"
        );
        assert_eq!(
            no_materializer.url(AuthorityClass::CallableHttp),
            Some(ADMITTER),
            "unnaming the materializer must not disturb another class"
        );

        let neither = host_credentials(Some(GUEST.to_owned()), None, None, None);
        assert_eq!(neither.url(AuthorityClass::ExecutorPlatform), None);
        assert_eq!(neither.url(AuthorityClass::CallableHttp), None);
        assert_eq!(neither.url(AuthorityClass::EventMaterializer), None);
        assert_eq!(
            neither.url(AuthorityClass::GuestSql),
            Some(GUEST),
            "the guest class keeps its own credential either way"
        );

        let materializer_only = host_credentials(None, None, None, Some(MATERIALIZER.to_owned()));
        assert_eq!(
            materializer_only.url(AuthorityClass::EventMaterializer),
            Some(MATERIALIZER)
        );
        assert_eq!(materializer_only.url(AuthorityClass::GuestSql), None);
    }

    #[test]
    fn session_configuration_requires_the_complete_public_trust_binding() {
        assert!(TestCli::try_parse_from(["host"]).is_ok());
        for arguments in [
            vec!["host", "--session-issuer", "https://identity.invalid"],
            vec!["host", "--session-jwks-ca", "/ca.pem"],
            vec!["host", "--session-instance-suffix", "k3m9x2p7"],
        ] {
            assert!(TestCli::try_parse_from(arguments).is_err());
        }
        let cli = TestCli::try_parse_from([
            "host",
            "--org",
            "acme",
            "--project",
            "receiving",
            "--session-issuer",
            "https://identity.invalid",
            "--session-jwks-ca",
            "/nonexistent/session-public-ca.pem",
            "--session-instance-suffix",
            "k3m9x2p7",
        ])
        .expect("complete session trust configuration");
        for database in [
            "wamn-db-other--receiving--dev--k3m9x2p7",
            "wamn-db-acme--other--dev--k3m9x2p7",
            "wamn-db-acme--receiving--prod--k3m9x2p7",
            "wamn-db-acme--receiving--dev--a1b2c3d4",
        ] {
            let error = session_verifier(
                &cli.args,
                "dev",
                &format!("postgres://reader:secret@localhost/{database}"),
            )
            .expect_err("the permission reader must target the exact audience database");
            assert_eq!(
                error.to_string(),
                "session audience and permission database differ"
            );
            assert!(!format!("{error:?}").contains("secret"));
        }
    }

    #[test]
    fn callable_http_configuration_is_not_authority_without_pat_demand() {
        assert_eq!(
            demanded_http_admitter_url(false, Some(ADMITTER.to_owned())),
            None,
            "an anonymous-only release must not name an ambient credential"
        );
        assert_eq!(
            demanded_http_admitter_url(true, Some(ADMITTER.to_owned())).as_deref(),
            Some(ADMITTER),
            "a PAT-protected release keeps the credential it requires"
        );
    }

    /// The R2 posture's first half: a host given no release pair was never given
    /// a release, so there is nothing to pull and nothing to refuse.
    #[tokio::test]
    async fn a_host_given_no_release_pair_carries_no_release() {
        let release = load_release(None, None, false, None, &[])
            .await
            .expect("no release pair is not a failure");
        assert!(
            release.is_none(),
            "a host with no release pair must carry no release rather than \
             loading one from somewhere else"
        );
    }

    /// The R2 posture's second half: a host told it serves a release and unable
    /// to configure the pull must fail startup before the host serves a release.
    ///
    /// The credential is the first thing the source reads, so an absent one
    /// refuses before any network I/O — which is what keeps this test isolated.
    #[tokio::test]
    async fn a_host_given_an_unusable_release_pair_refuses() {
        let auth_file = Path::new("/nonexistent/wamn-registry-auth.json");
        let error = load_release(
            Some(RELEASE_BASE),
            Some(RELEASE_DIGEST),
            false,
            Some(auth_file),
            &[],
        )
        .await
        .expect_err("an unusable release pair must refuse");
        let message = format!("{error:#}");
        assert!(
            message.contains("registry-credentials-unreadable"),
            "the refusal must name the transfer invariant it could not satisfy: {message}"
        );
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
        let mut cli = TestCli::try_parse_from(arguments).expect("the native memory flags parse");
        assert_eq!(cli.args.guest_memory_mode, "count");
        for mode in ["enforce", "off"] {
            let invalid = arguments.iter().map(|argument| {
                if *argument == "--guest-memory-mode=count" {
                    format!("--guest-memory-mode={mode}")
                } else {
                    (*argument).to_owned()
                }
            });
            let error = TestCli::try_parse_from(invalid).unwrap_err();
            assert!(error.to_string().contains("--guest-memory-mode"));
        }
        assert_eq!(
            host_memory(&cli.args).expect("memory settings resolve"),
            HostMemoryBudgets {
                max_guest_memory: 512 << 20,
                default_heap_memory: 64 << 20,
                core_instances: 7,
            },
            "all native memory settings must reach the engine"
        );
        cli.args.max_guest_memory = Some("  ".to_owned());
        assert!(
            host_memory(&cli.args)
                .unwrap_err()
                .to_string()
                .contains("max-guest-memory must not be blank")
        );
        cli.args.max_guest_memory = None;
        cli.args.default_heap_memory = "  ".to_owned();
        assert!(
            host_memory(&cli.args)
                .unwrap_err()
                .to_string()
                .contains("default-heap-memory must not be blank")
        );
    }

    #[test]
    fn the_removed_runtime_flags_are_not_accepted() {
        for flag in ["--pool-slots", "--pool-memory-cap-bytes", "--epoch-tick-ms"] {
            assert!(
                TestCli::try_parse_from(["wamn-host", flag, "1"]).is_err(),
                "legacy runtime flag {flag} must not remain an accepted contract"
            );
        }
    }

    /// The third state a base-plus-digest pair invents — one half without the
    /// other — is refused by the parser, so it never reaches [`load_release`] and
    /// never becomes a pod that quietly serves no release.
    #[test]
    fn a_host_given_half_a_release_pair_refuses_to_parse() {
        for half in [
            ["--release-artifact-base", RELEASE_BASE],
            ["--release-manifest-digest", RELEASE_DIGEST],
        ] {
            let error = TestCli::try_parse_from(["wamn-host", half[0], half[1]])
                .expect_err("half a release pair must refuse to parse");
            let message = error.to_string();
            assert!(
                message.contains("--release-artifact-base")
                    && message.contains("--release-manifest-digest"),
                "the refusal must name both halves of the pair: {message}"
            );
        }
    }

    /// Both halves together parse, so the refusal above is the missing half and
    /// not the pair itself.
    #[test]
    fn a_host_given_both_release_halves_parses() {
        let cli = TestCli::try_parse_from([
            "wamn-host",
            "--release-artifact-base",
            RELEASE_BASE,
            "--release-manifest-digest",
            RELEASE_DIGEST,
        ])
        .expect("a complete release pair parses");
        assert_eq!(
            cli.args.release_artifact_base.as_deref(),
            Some(RELEASE_BASE)
        );
        assert_eq!(
            cli.args.release_manifest_digest.as_deref(),
            Some(RELEASE_DIGEST)
        );
    }
}
