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

mod commands;

use wamn_catalog::ServingManifest;
use wamn_control_provision::session_target::session_audience;
use wamn_control_provision::{SystemReader, project_env_database_name, system_reader_login};
use wamn_engine::artifact_source::ArtifactSource;
use wamn_engine::engine::{
    DEFAULT_CORE_INSTANCES, build_engine_with_host_memory,
    build_engine_with_host_memory_and_compilation_cache,
};
use wamn_engine::flow_http_routing::{
    FlowHttpRouting, requires_pat_route_authentication, requires_session_route_authentication,
};
use wamn_engine::release_manifest::{LoadedRelease, ReleaseTarget, release_label};
use wamn_execution_host::{
    HostRouteDelivery, HostRouteHandlers, OperationHost, OperationHostConfig,
};
use wamn_identity_protocol::verifier::SessionVerifier;
use wamn_package_artifact::release_manifest_source::ReleaseManifestSource;
use wamn_registry::{Triple, environment_nats_user, project_env_key};
use wamn_runtime::auth_cache::replicate;
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::connection_credentials::ConnectionCredentials;
use wamn_runtime::connection_facts::ConnectionFacts;
use wamn_runtime::connection_memory::{self, ConnectionMemory};
use wamn_runtime::plugins::connection_http::transport::HttpTransport;
use wamn_runtime::plugins::route_authentication::{
    EnvironmentStatusReader, PlatformRouteAuthenticator,
};
use wamn_runtime::plugins::wamn_jetstream::WamnJetstreamConfig;
use wamn_runtime::plugins::wamn_postgres::AuthorityClass;
use wamn_runtime::plugins::{ClassCredentials, WamnJetstream, WamnPostgres};
use wamn_runtime::session_keys::{IssuerKeys, IssuerKeysConfig};
use wamn_workflow::{DEFAULT_QUEUE_LEASE_TTL_MS, QueueService, QueueServiceConfig, RouterDriver};

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

    /// The workload grace, which the release chart renders: the time to stop
    /// services and answer operator stops before the host closes ingress.
    /// Components stop after HTTP drains. This must be shorter than the drain bound.
    #[arg(
        long,
        env = "WAMN_MATERIALIZER_DRAIN_BOUND_SECONDS",
        default_value = "10"
    )]
    pub materializer_drain_bound_seconds: u64,

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
    pub user_config: Option<PathBuf>,

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

    /// Duration of a durable queue lease before another replica may recover it.
    #[arg(long, env = "WAMN_QUEUE_LEASE_TTL_MS", default_value_t = DEFAULT_QUEUE_LEASE_TTL_MS)]
    pub queue_lease_ttl_ms: u64,

    /// The environment of this host's release target, and nothing else. The
    /// operator chart passes the pod namespace first; a release passes the
    /// environment name again in its `extraArgs`, and the last value wins
    /// (R1, R6).
    #[arg(long = "environment", overrides_with = "environment")]
    pub environment: Option<String>,

    /// The one application this host serves, as canonical JSON:
    /// application name to `{"packages": [...]}`. A release passes it in its
    /// `extraArgs` beside `--environment`. A host that serves a release from
    /// the registry refuses to start without it.
    #[arg(long = "applications")]
    pub applications: Option<String>,

    /// Address for the workload HTTP server
    #[arg(long = "http-addr")]
    pub http_addr: Option<SocketAddr>,

    /// Address for native file transfers on release hosts.
    #[arg(long, env = "WAMN_FILES_ADDR")]
    pub files_addr: Option<SocketAddr>,

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

    /// Exact reviewed component digests trusted for reuse by the deployment owner.
    #[arg(
        long,
        env = "WAMN_TRUSTED_WARM_COMPONENT_DIGESTS",
        value_delimiter = ','
    )]
    pub trusted_warm_component_digest: Vec<String>,

    /// Native instances retained per eligible component, independent of admission limits.
    #[arg(long, env = "WAMN_COMPONENT_POOL_SIZE", default_value_t = 1)]
    pub component_pool_size: i32,

    /// Native idle reclamation window for eligible components.
    #[arg(
        long,
        env = "WAMN_COMPONENT_RECLAIM_WINDOW_SECONDS",
        default_value_t = 60
    )]
    pub component_reclaim_window_seconds: i32,

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

    /// Guest memory accounting mode. Count records growth without enforcing the guest budget.
    #[arg(
        long,
        env = "WASH_GUEST_MEMORY_MODE",
        default_value = "count",
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

    /// OAuth envelope provider as JSON: local, or gcp with project and region.
    #[arg(long, env = "WAMN_OAUTH_ENCRYPTION")]
    pub oauth_encryption: Option<String>,

    /// Explicit registry/repository holding digest-addressed node components.
    #[arg(long, env = "WAMN_COMPONENT_ARTIFACT_BASE")]
    pub component_artifact_base: Option<String>,

    /// Projected `.dockerconfigjson` file for the component registry.
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: Option<PathBuf>,

    /// Pull with a token from the GKE metadata server of this pod's Workload
    /// Identity, asked for at each pull, in place of `--registry-auth-file`.
    #[arg(
        long,
        env = "WAMN_REGISTRY_TOKEN_METADATA",
        conflicts_with = "registry_auth_file"
    )]
    pub registry_token_metadata: bool,

    /// Targeted system connection reader for node credentials and signature routes.
    #[arg(long, env = "WAMN_CONNECTION_STORE_URL")]
    pub connection_store_url: Option<String>,

    /// Project whose platform pool and credentials this host uses.
    #[arg(long, env = "WAMN_PROJECT", default_value = "default")]
    pub project: String,

    /// Organization owning this host's project.
    ///
    /// Required only when the loaded release carries a PAT-protected HTTP
    /// route; it targets the identity-reader URL.
    #[arg(long, env = "WAMN_ORG")]
    pub org: Option<String>,

    /// Trusted HTTPS issuer for session routes, never discovered from a token.
    #[arg(long, env = "WAMN_SESSION_ISSUER", requires = "session_jwks_ca")]
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

    /// Optional application data search path installed at node checkout.
    #[arg(long, env = "WAMN_SCHEMA")]
    pub schema: Option<String>,

    /// Production outbound ceiling for connection-backed HTTP effects.
    #[arg(long, env = "WAMN_ALLOWED_HOSTS", value_delimiter = ',')]
    pub allowed_hosts: Vec<String>,
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
/// The registry credential of a release-backed host: exactly one of
/// `--registry-auth-file` and `--registry-token-metadata`.
enum RegistryPullCredential<'a> {
    AuthFile(&'a Path),
    TokenMetadata,
}

impl<'a> RegistryPullCredential<'a> {
    /// Clap refuses both flags together; this refuses neither.
    fn select(auth_file: Option<&'a Path>, token_metadata: bool) -> anyhow::Result<Self> {
        if token_metadata {
            return Ok(Self::TokenMetadata);
        }
        auth_file.map(Self::AuthFile).context(
            "a release-backed host requires --registry-auth-file or --registry-token-metadata",
        )
    }
}

/// The node-component registry is deliberately a separate explicit argument.
/// A release artifact establishes release identity; it never implies where
/// digest-addressed component bytes may be pulled from.
async fn load_release(
    artifact_base: Option<&str>,
    manifest_digest: Option<&str>,
    insecure_registry: bool,
    registry_auth_file: Option<&Path>,
    registry_token_metadata: bool,
    ca_paths: &[PathBuf],
    target: anyhow::Result<ReleaseTarget>,
) -> anyhow::Result<Option<Arc<LoadedRelease>>> {
    let (Some(artifact_base), Some(manifest_digest)) = (artifact_base, manifest_digest) else {
        return Ok(None);
    };
    let target = target?;
    let source = match RegistryPullCredential::select(registry_auth_file, registry_token_metadata)?
    {
        RegistryPullCredential::AuthFile(path) => {
            ReleaseManifestSource::new(artifact_base, insecure_registry, path)
        }
        RegistryPullCredential::TokenMetadata => {
            ReleaseManifestSource::with_registry_token_metadata(artifact_base, insecure_registry)
        }
    }
    .context("configure the release-manifest registry")?
    .with_ca_paths(ca_paths)
    .context("trust the configured OCI CA bundles for the release pull")?;
    let canonical_bytes = source
        .pull_verified(manifest_digest)
        .await
        .context("pull the serving release manifest")?;
    let origin = format!("{artifact_base}@{manifest_digest}");
    let loaded_release = LoadedRelease::load_canonical_bytes(&canonical_bytes, &origin, target)
        .map_err(|error| {
            anyhow::anyhow!(
                "serving release manifest {origin} is unusable ({:?}): {error}",
                error.error_type()
            )
        })?;
    // Shared by reference-count rather than by borrow: every release-conditioned
    // plugin and the router driver are `Arc`-owned, so none can hold a lifetime
    // tied to `run`'s stack. One allocation remains the process's only manifest.
    Ok(Some(Arc::new(loaded_release)))
}

/// The environment key and declared applications that this host serves (R1).
/// The manifest names none of them, so they come from `--org`, `--project`,
/// `--environment` and `--applications`, and the environment key is
/// [`project_env_key`] of the first three.
fn release_target(args: &HostArgs) -> anyhow::Result<ReleaseTarget> {
    let org = args
        .org
        .as_deref()
        .filter(|org| !org.is_empty())
        .context("a host that serves a release requires --org/WAMN_ORG")?;
    anyhow::ensure!(
        !args.project.is_empty(),
        "a host that serves a release requires a nonempty --project/WAMN_PROJECT"
    );
    let environment = args
        .environment
        .as_deref()
        .filter(|environment| !environment.is_empty())
        .context("a host that serves a release requires --environment")?;
    let applications = match args.applications.as_deref() {
        None => wamn_catalog::EnvironmentApplications::new(),
        Some(applications) => serde_json::from_str(applications)
            .context("--applications is not the JSON of the declared applications")?,
    };
    Ok(ReleaseTarget::new(
        project_env_key(org, &args.project, environment),
        environment,
    )
    .with_applications(applications))
}

/// The target of a host that serves a release from the registry. Env apply
/// renders `--applications` for every such host, and a host without it
/// refuses to start.
fn served_release_target(args: &HostArgs) -> anyhow::Result<ReleaseTarget> {
    anyhow::ensure!(
        args.applications.is_some(),
        "a host that serves a release requires --applications"
    );
    let target = release_target(args)?;
    anyhow::ensure!(
        target.applications.len() == 1,
        "an application host requires exactly one application"
    );
    Ok(target)
}

/// The `host.plugins` entries of the chart's host configuration file. Only
/// the native entries the release chart declares are read: a plugin with an
/// image, a file, `configFrom` or `secretFrom` is refused.
#[derive(serde::Deserialize)]
struct UserConfig {
    host: UserHost,
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
    administration_url: Option<String>,
) -> ClassCredentials {
    let credentials = database_url.map_or_else(ClassCredentials::default, |input| {
        let config = wamn_postgres::configuration(&input)
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
        match config {
            Some(config) if config.get("compiled").is_some() && config.get("dynamic").is_some() => {
                ClassCredentials::default()
                    .with_class(AuthorityClass::GuestSql, config["compiled"].to_string())
                    .with_class(AuthorityClass::DynamicSql, config["dynamic"].to_string())
            }
            _ => ClassCredentials::default().with_class(AuthorityClass::GuestSql, input),
        }
    });
    let credentials = match executor_platform_url {
        Some(platform) => credentials.with_class(AuthorityClass::ExecutorPlatform, platform),
        None => credentials.without_class(AuthorityClass::ExecutorPlatform),
    };
    let credentials = match http_admitter_url {
        Some(admitter) => credentials.with_class(AuthorityClass::CallableHttp, admitter),
        None => credentials.without_class(AuthorityClass::CallableHttp),
    };
    let credentials = match event_materializer_url {
        Some(materializer) => {
            credentials.with_class(AuthorityClass::EventMaterializer, materializer)
        }
        None => credentials.without_class(AuthorityClass::EventMaterializer),
    };
    match administration_url {
        Some(administration) => {
            credentials.with_class(AuthorityClass::Administration, administration)
        }
        None => credentials.without_class(AuthorityClass::Administration),
    }
}

fn declares_connection_requirement(manifest: &ServingManifest) -> bool {
    manifest
        .components
        .iter()
        .any(|component| !component.descriptor.connections.is_empty())
}

/// Name the callable-HTTP credential for route authorization or connection resolution.
/// A host with neither demand ignores an ambient URL rather than acquiring its pool.
fn demanded_http_admitter_url(
    authenticated_routes: bool,
    connection_requirements: bool,
    configured_url: Option<String>,
) -> Option<String> {
    if authenticated_routes || connection_requirements {
        configured_url
    } else {
        None
    }
}

fn require_queue_admission(
    released: bool,
    guest_url: Option<&str>,
    executor_platform_url: Option<&str>,
) -> anyhow::Result<bool> {
    let complete = guest_url.is_some() && executor_platform_url.is_some();
    anyhow::ensure!(
        !released || complete,
        "a released host requires both WAMN_PG_URL and WAMN_EXECUTOR_PLATFORM_PG_URL for durable queue admission"
    );
    Ok(complete)
}

fn session_verifier(
    args: &HostArgs,
    environment: &str,
    http_admitter_url: &str,
) -> anyhow::Result<SessionVerifier<IssuerKeys>> {
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
    let audience = session_audience(&Triple::new(org, environment, &args.project), suffix)?;
    let database = project_env_database_name(org, &args.project, environment, suffix);
    // Compare trusted deployment coordinates, not anything carried by a token.
    // Parsing failures never attach the credential-bearing URL to diagnostics.
    let (_, configured_database) = wamn_postgres::identity(http_admitter_url)?;
    anyhow::ensure!(
        configured_database == database,
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
    let keys = IssuerKeys::new(IssuerKeysConfig::new(issuer, endpoint.as_str(), &ca)?);
    SessionVerifier::new(keys, org, &audience).context("configure the host session verifier")
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
/// The data client and its `name`: the connection of the host's own code by the
/// data URL with the environment certificate (docs/plan/alignment.md R5, R8).
/// Its replies use the inbox of `user`, the one inbox prefix that user may
/// read. It reconnects and starts like the scheduler client.
async fn connect_data_nats(
    args: &HostArgs,
    name: &str,
    user: Option<&str>,
) -> anyhow::Result<async_nats::Client> {
    let mut opts = async_nats::ConnectOptions::new()
        .name(name)
        .max_reconnects(None)
        .client_capacity(SCHEDULER_NATS_BUFFER);
    if let Some(user) = user {
        opts = opts.custom_inbox_prefix(wamn_registry::name::nats_subject(&[
            &wamn_registry::name::nats_token("_INBOX_", (user).as_ref(), ""),
        ]));
    }
    opts = wamn_broker::nats::tls_options(
        opts,
        args.data_nats_tls_ca.as_deref(),
        args.data_nats_tls_cert.as_deref(),
        args.data_nats_tls_key.as_deref(),
        None,
    )?;
    if args.data_nats_tls_first {
        opts = opts.tls_first();
    }
    wamn_broker::nats::connect(
        opts,
        &args.data_nats_url,
        wamn_broker::nats::InitialConnect::RetryTransport,
    )
    .await
}

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
    anyhow::ensure!(
        args.materializer_drain_bound_seconds < args.drain_bound_seconds,
        "materializer_drain_bound_seconds must be shorter than drain_bound_seconds"
    );
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
    // pair from this object — including the lease-time recording that
    // wamn-0h0g.15.103 repoints at it. A component that bound before the loaded release
    // existed would have no pair to record. Building it here, ahead of the NATS
    // connections, the engine and every plugin, makes that ordering impossible to
    // get wrong, and makes a host that cannot verify its release refuse before it
    // opens a socket. The ordering guard in
    // tests/conformance/src/runtime_policy.rs keeps the load ahead of binding.
    //
    // It also runs after the CA install above, which is why the release pull can
    // reach a registry behind the chart's own CA.
    let release = load_release(
        args.release_artifact_base.as_deref(),
        args.release_manifest_digest.as_deref(),
        args.allow_insecure_registries,
        args.registry_auth_file.as_deref(),
        args.registry_token_metadata,
        &args.oci_ca_paths,
        served_release_target(&args),
    )
    .await?;
    let application = release.as_ref();
    let pat_routes = application
        .is_some_and(|loaded_release| requires_pat_route_authentication(loaded_release.manifest()));
    let session_routes = application.is_some_and(|loaded_release| {
        requires_session_route_authentication(loaded_release.manifest())
    });
    let signature_routes = application.is_some_and(|loaded_release| {
        loaded_release
            .manifest()
            .every_served_attachment()
            .any(|(_, attachment)| {
                matches!(
                    wamn_catalog::parse_attachment_auth_policy(attachment.auth_policy()),
                    Some(wamn_catalog::AttachmentAuthPolicy::Signature { .. })
                )
            })
    });
    let connection_requirements = application
        .is_some_and(|loaded_release| declares_connection_requirement(loaded_release.manifest()));
    let http_admitter_url = std::env::var("WAMN_HTTP_ADMITTER_PG_URL")
        .ok()
        .filter(|url| !url.is_empty());
    let session_verifier = if session_routes {
        let loaded_release = release
            .as_ref()
            .expect("a session route belongs to a loaded release");
        Some(session_verifier(
            &args,
            &loaded_release.target().environment,
            http_admitter_url
                .as_deref()
                .context("a session route requires WAMN_HTTP_ADMITTER_PG_URL")?,
        )?)
    } else {
        None
    };

    // The release pull above is what makes the PAT requirement knowable. Once
    // it is known, settle every targeted input before opening the identity, NATS,
    // project-database, or ingress sockets. A release-less or anonymous-only
    // host takes the absent arm and acquires no identity-reader connection.
    let (route_auth_target, identity_reader) = if pat_routes || session_routes {
        let loaded_release = release
            .as_ref()
            .expect("an authenticated route was found only inside a loaded release");
        let org = args
            .org
            .as_deref()
            .filter(|org| !org.is_empty())
            .context("an authenticated route requires --org/WAMN_ORG")?;
        let project = (!args.project.is_empty())
            .then_some(args.project.as_str())
            .context("an authenticated route requires a nonempty --project/WAMN_PROJECT")?;
        let system_url = std::env::var("WAMN_SYSTEM_URL")
            .ok()
            .filter(|url| !url.is_empty())
            .context("an authenticated route requires WAMN_SYSTEM_URL")?;
        http_admitter_url
            .as_deref()
            .context("an authenticated route requires WAMN_HTTP_ADMITTER_PG_URL")?;
        let (login, database) = wamn_postgres::identity(&system_url)?;
        anyhow::ensure!(
            login
                == system_reader_login(
                    SystemReader::Identity,
                    org,
                    project,
                    &loaded_release.target().environment,
                    &database
                ),
            "identity reader targets another environment"
        );
        let client = wamn_postgres::pool(&system_url, 4).await?;
        (Some(org.to_owned()), Some(client))
    } else {
        (None, None)
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

    let scheduler_nats_client =
        connect_scheduler_nats(&args, args.host_name.as_deref().unwrap_or(&router_owner)).await?;
    // The data connection carries the environment identity.
    let data_nats_user = application
        .map(|release| environment_nats_user(&release.target().project_env_key))
        .or_else(|| {
            args.org
                .as_deref()
                .zip(args.environment.as_deref())
                .map(|(org, environment)| {
                    environment_nats_user(&project_env_key(org, &args.project, environment))
                })
        });
    let data_nats_client = connect_data_nats(
        &args,
        args.host_name.as_deref().unwrap_or(&router_owner),
        data_nats_user.as_deref(),
    )
    .await?;
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
    // wamn-0h0g.22.8.3: the host NAMES its own credential sources here rather
    // than letting the config layer pick one up implicitly. deploy/platform
    // injects WAMN_PG_URL via secretKeyRef (values-host-default.yaml),
    // so the environment is the transport; reading it at
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
    // platform.permission read that PAT/session routes require. Connection
    // resolution also uses this class. The manifest-derived condition retains the
    // pool for either demand; a host with neither retains the absent arm.
    // EventMaterializer uses the same explicit-source rule: only the dedicated
    // URL names that class, and absence removes the shared guest placeholder.
    let executor_platform_url = std::env::var("WAMN_EXECUTOR_PLATFORM_PG_URL")
        .ok()
        .filter(|url| !url.is_empty());
    let event_materializer_url = std::env::var("WAMN_EVENT_MATERIALIZER_PG_URL")
        .ok()
        .filter(|url| !url.is_empty());
    let http_admitter_url = demanded_http_admitter_url(
        pat_routes || session_routes || signature_routes,
        connection_requirements,
        http_admitter_url,
    );
    let guest_url = std::env::var("WAMN_PG_URL")
        .ok()
        .filter(|url| !url.is_empty());
    // An application host refuses every route while its environment is
    // inactive, from its row read as `wamn_app`
    // (docs/architecture/execution.md#routing-and-availability).
    let reads_environment_status = application.is_some() && guest_url.is_some();
    let queue_credentials_complete = require_queue_admission(
        application.is_some(),
        guest_url.as_deref(),
        executor_platform_url.as_deref(),
    )?;
    // The host routes of an application release read and write the
    // authorization relations under their own credential
    // (docs/architecture/data-access.md#actors).
    let administration_url = std::env::var("WAMN_ADMINISTRATION_PG_URL")
        .ok()
        .filter(|url| !url.is_empty() && application.is_some());
    let postgres_credentials = if guest_url.is_some()
        || executor_platform_url.is_some()
        || http_admitter_url.is_some()
        || event_materializer_url.is_some()
        || administration_url.is_some()
    {
        Some(host_credentials(
            guest_url.clone(),
            executor_platform_url.clone(),
            http_admitter_url,
            event_materializer_url,
            administration_url.clone(),
        ))
    } else {
        None
    };
    let mut postgres = WamnPostgres::from_env_for_project(&args.project, postgres_credentials)
        .context("wamn:postgres plugin init")?;
    if (pat_routes || session_routes || signature_routes)
        && let Some(release) = &release
    {
        postgres
            .configure_revocation(
                &args.project,
                &release
                    .target()
                    .applications
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
            .await?;
    }
    let postgres = Arc::new(postgres);
    let logging = Arc::new(plugin::wasi_logging::TracingLogger::default());
    let http_transport = Arc::new(HttpTransport::new().context("HTTP transport init")?);
    let mut file_connection_reader = None;
    let credentials = match (&application, &args.connection_store_url) {
        (Some(release), Some(url)) => {
            let org = args.org.as_deref().context("host org is absent")?;
            let (login, database) = wamn_postgres::identity(url)?;
            anyhow::ensure!(
                login
                    == system_reader_login(
                        SystemReader::Connection,
                        org,
                        &args.project,
                        &release.target().environment,
                        &database
                    ),
                "connection reader targets another environment"
            );
            let client = wamn_postgres::pool(url, 4).await?;
            file_connection_reader = Some(client.clone());
            ConnectionCredentials::from_database(client, org)
        }
        _ => ConnectionCredentials::empty(),
    };
    let credentials = Arc::new(credentials);
    let resource_leases = Arc::new(
        wamn_runtime::resource_lease::ResourceLeases::new(args.connection_store_url.as_deref())
            .context("configure resource lease session")?,
    );
    let memory =
        Arc::new(ConnectionMemory::default().with_resource_leases(Arc::clone(&resource_leases)));
    let changes = connection_memory::replicate(
        &data_nats_client,
        application.map(|release| release.target().project_env_key.as_str()),
        Arc::clone(&memory),
    );
    let oauth_encryption = args
        .oauth_encryption
        .as_deref()
        .map(|value| {
            let configuration =
                serde_json::from_str(value).context("decode WAMN_OAUTH_ENCRYPTION")?;
            wamn_runtime::oauth_encryption::Envelope::open(&configuration)
                .map(Arc::new)
                .context("open OAuth envelope encryption")
        })
        .transpose()?;
    let facts = Arc::new(
        ConnectionFacts::new(
            memory,
            changes,
            Arc::clone(&credentials),
            Arc::clone(&postgres),
            application.map_or("", |release| release.target().project_env_key.as_str()),
            &args.project,
            application.map_or("", |release| release.target().environment.as_str()),
        )
        .with_oauth_encryption(oauth_encryption.clone()),
    );

    let integration_registry = if let Some(base) = &args.component_artifact_base {
        let registry_name = wamn_catalog::parse_component_artifact_base(base)?;
        let credentials = match RegistryPullCredential::select(args.registry_auth_file.as_deref(), args.registry_token_metadata)? {
            RegistryPullCredential::AuthFile(path) => wamn_package_artifact::registry_credentials::RegistryCredentialSource::Fixed(
                wamn_package_artifact::registry_credentials::read_registry_credentials(path, registry_name.registry())?),
            RegistryPullCredential::TokenMetadata => wamn_package_artifact::registry_credentials::RegistryCredentialSource::MetadataServer,
        };
        Some((
            wamn_package_artifact::PackageRegistry {
                artifact_base: base.clone(),
                credentials: None,
                registry_auth_file: args.registry_auth_file.clone().unwrap_or_default(),
                insecure_registry: args.allow_insecure_registries,
                oci_ca_paths: args.oci_ca_paths.clone(),
            },
            credentials,
        ))
    } else {
        None
    };
    let integrations = match (application, &integration_registry) {
        (Some(release), Some((registry, credentials))) => Some(Arc::new(
            wamn_runtime::integrations::Integrations::new(
                Arc::clone(&facts),
                release,
                &postgres,
                &args.project,
                registry.clone(),
                credentials.clone(),
            )
            .await?,
        )),
        _ => None,
    };
    let operations = match application {
        Some(release) => {
            let artifact_base = args
                .component_artifact_base
                .as_deref()
                .context("a serving host requires --component-artifact-base")?;
            let source_config = ComponentArtifactSourceConfig::new(
                artifact_base,
                args.allow_insecure_registries,
                Duration::from_secs(30),
            )?;
            let source_config = match RegistryPullCredential::select(
                args.registry_auth_file.as_deref(),
                args.registry_token_metadata,
            )? {
                RegistryPullCredential::AuthFile(path) => source_config
                    .with_registry_auth_file(path)
                    .context("load component registry pull credential")?,
                RegistryPullCredential::TokenMetadata => {
                    source_config.with_registry_token_metadata()
                }
            }
            .with_ca_paths(&args.oci_ca_paths)
            .context("trust the configured OCI CA bundles for component pulls")?;
            let source = Arc::new(
                ComponentArtifactSource::new(source_config)
                    .context("configure the component artifact registry client")?,
            );
            let source: Arc<dyn ArtifactSource> = source;
            let allowed_hosts = args
                .allowed_hosts
                .iter()
                .map(|value| value.parse())
                .collect::<Result<Vec<_>, _>>()?;
            Some(Arc::new(OperationHost::new(
                Arc::clone(&engine),
                Arc::clone(&postgres),
                Arc::clone(&http_transport),
                Arc::clone(&facts),
                Arc::clone(&logging),
                allowed_hosts.into(),
                Arc::clone(release),
                source,
                OperationHostConfig {
                    project: args.project.clone(),
                    schema: args.schema.clone(),
                    owner_prefix: router_owner.clone(),
                    warm_reuse: wamn_engine::warm_reuse::WarmReuse::new(
                        &args.trusted_warm_component_digest,
                        args.component_pool_size,
                        args.component_reclaim_window_seconds,
                    )?,
                },
            )?))
        }
        None => None,
    };
    if let (Some(operations), Some(integrations)) = (&operations, &integrations) {
        operations.set_integrations(Arc::clone(integrations))?;
    }
    let router_driver = operations
        .as_ref()
        .map(|operations| Arc::new(RouterDriver::new(Arc::clone(operations))));
    let host_config = HostConfig {
        allow_oci_insecure: args.allow_insecure_registries,
        oci_pull_timeout: Some(Duration::from_secs(30)),
        oci_cache_dir: args.oci_cache_dir.clone(),
        oci_ca_paths: args.oci_ca_paths.clone(),
    };
    let jetstream_config = WamnJetstreamConfig::from_env().with_nats(data_nats_client.clone());
    let jetstream = Arc::new(WamnJetstream::new(jetstream_config));
    jetstream
        .activate_events()
        .await
        .map_err(|error| anyhow::anyhow!("activate declared event streams: {error:?}"))?;
    let flow_http = FlowHttpRouting::new(release.clone());
    // The auth memory is the replica of the NATS KV bucket wamn-auth, and
    // the host deletes the keys of its own writes there
    // (docs/architecture/execution.md#auth-memory).
    let project_env_key = release
        .as_ref()
        .map(|release| release.target().project_env_key.clone());
    let auth_changes = replicate(
        &data_nats_client,
        project_env_key.as_deref(),
        Arc::clone(postgres.auth_cache()),
    );
    // The host always installs its authenticator, even an empty one, so a
    // protected route refuses the same way on every host.
    let authenticator = PlatformRouteAuthenticator::new(
        Arc::clone(&postgres),
        Arc::clone(&facts),
        args.project.clone(),
        identity_reader
            .as_ref()
            .map(|reader| (reader.clone(), session_verifier)),
    );
    let flow_http = if reads_environment_status {
        flow_http
            .with_authenticator(Arc::new(authenticator))
            .with_environment_status(Arc::new(EnvironmentStatusReader::new(
                Arc::clone(&postgres),
                args.project.clone(),
            )))
    } else {
        flow_http.with_authenticator(Arc::new(authenticator))
    };

    let flow_http = Arc::new(flow_http);
    let files = application.map(|release| {
        let connections = Arc::new(wamn_execution_host::file_route::RegistryFileConnections {
            client: file_connection_reader,
            release: Arc::clone(release),
            facts: Arc::clone(&facts),
            org: args.org.clone().unwrap_or_default(),
            project: args.project.clone(),
            environment: release.target().environment.clone(),
        });
        let blobstore = Arc::new(
            wamn_runtime::plugins::wamn_blobstore::plugin::WamnBlobstore::new(
                Arc::clone(&postgres),
                Arc::clone(&facts),
                args.project.clone(),
                Arc::clone(release),
            ),
        );
        Arc::new(wamn_execution_host::file_route::FileRoutes::new(
            Arc::clone(release),
            Arc::clone(&postgres),
            blobstore,
            connections,
            args.project.clone(),
            administration_url.clone(),
        ))
    });

    if let (Some(operations), Some(files)) = (&operations, &files) {
        operations.set_file_routes(Arc::clone(files))?;
    }

    let mut plugins: Vec<Arc<dyn plugin::HostPlugin>> = vec![
        Arc::new(
            plugin::wasi_config::DynamicConfig::builder()
                .copy_environment(true)
                .build(),
        ),
        logging,
        Arc::new(plugin::wasi_otel::WasiOtel::default()),
        // Pool config from WAMN_PG_URL + the WAMN_PG_* tuning env; without a URL
        // the plugin still links and returns connection-unavailable on use.
        Arc::<WamnPostgres>::clone(&postgres),
        // l5i9.17: the wamn:jetstream plugin (E10), first bound by the
        // Service-first materializer. It uses the host's data connection.
        // wamn-0h0g.15.95: READER 4 of the loaded release. Delivery is conditioned on the serving
        // release's registration projection, so an event whose registration
        // identity is not in this release never reaches a component. The plugin
        // takes the loaded manifest — it does not load one.
        Arc::<WamnJetstream>::clone(&jetstream),
        // wamn-0h0g.15.96: READER 3 of the loaded release. Route projection stays wholly
        // in-memory; a PAT-protected route additionally carries the targeted
        // identity reader and preloads exact grants from the existing
        // callable-HTTP project pool during authentication.
        Arc::<FlowHttpRouting>::clone(&flow_http),
    ];

    let application_http = if let Some(operations) = &operations {
        Some(Arc::new(
            HostRouteDelivery::new(
                Arc::clone(&operations.release),
                HostRouteHandlers::Application {
                    postgres: Arc::clone(&postgres),
                    project: args.project.clone(),
                    identity: route_auth_target
                        .as_ref()
                        .zip(identity_reader.as_ref())
                        .map(|(org, client)| (client.clone(), org.clone())),
                    changes: auth_changes.clone(),
                },
            )
            .with_files(files.clone()),
        ))
    } else {
        None
    };
    let queue = match (&router_driver, &release) {
        (Some(driver), Some(release)) if queue_credentials_complete => Some(
            QueueService::bind(
                Arc::clone(driver),
                Arc::clone(&postgres),
                Arc::clone(&jetstream),
                release,
                QueueServiceConfig {
                    project: args.project.clone(),
                    runner: router_owner,
                    lease_ttl_ms: args.queue_lease_ttl_ms,
                },
            )
            .await
            .context("bind durable queue execution")?,
        ),
        _ => None,
    };

    let native_event_bindings = if let Some(path) = &args.user_config {
        let source = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("read the host configuration file {}", path.display()))?;
        plugins.push(Arc::new(
            plugin::wasmcloud_nats::WasmcloudNats::new()
                .with_memory_budget(host_memory.max_guest_memory)
                .with_lattice_prefixes(vec![
                    format!("{}.", wash_runtime::washlet::HOST_API_PREFIX),
                    format!("{}.", wash_runtime::washlet::OPERATOR_API_PREFIX),
                ]),
        ));
        Some(plugin_bindings(&args, &source)?)
    } else {
        None
    };

    if let (Some(integrations), Some(release)) = (&integrations, application) {
        plugins.push(Arc::new(wamn_runtime::integrations::IntegrationIngress {
            integrations: Arc::clone(integrations),
            release: Arc::clone(release),
        }));
    }

    // Count the same plugins supplied to the native host, including the optional bridge.
    let cleanup_budget = host_cleanup_budget(plugins.len())?;
    let mut host_builder = wash_runtime::host::HostBuilder::default();
    for plugin in plugins {
        host_builder = host_builder.with_plugin(plugin)?;
    }
    if let Some(bindings) = native_event_bindings {
        host_builder = host_builder.with_plugin_bindings(bindings);
    }
    // The heartbeat carries the release so that `hostSelector` can tell this
    // host from a host of another release in the same group (docs/architecture/naming.md#environment-names).
    if let Some(digest) = &args.release_manifest_digest {
        let label =
            release_label(digest).context("--release-manifest-digest is not a sha256 digest")?;
        host_builder = host_builder.with_label("wamn.release", label);
    }
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
    let mut schedule_tasks = tokio::task::JoinSet::new();
    if let Some(release) = application {
        let events = Arc::clone(&jetstream);
        let postgres = Arc::clone(&postgres);
        let args_project = args.project.clone();
        schedule_tasks.spawn(serve_application_events(
            Arc::clone(release),
            stopping.clone(),
            async move |release: &Arc<LoadedRelease>| {
                if let Err(error) = wamn_workflow::event_admission::admit_application_events(
                    &events,
                    &postgres,
                    &args_project,
                    release,
                )
                .await
                {
                    tracing::warn!(?error, "application event admission failed");
                }
            },
        ));
    }
    // Each host admits schedules with its own loaded application release.
    if let Some(release) = application {
        let schedules = wamn_workflow::schedule::PostgresSchedules::new(
            Arc::clone(&postgres),
            &args.project,
            Arc::clone(release),
        );
        let schedule_stopping = stopping.clone();
        let task = schedule_tasks.spawn(async move {
            if let Err(error) = wamn_workflow::schedule::serve(&schedules, schedule_stopping).await
            {
                tracing::warn!(%error, "workflow schedule task stopped");
            }
        });
        probe_state.register(Arc::new(ScheduleProbe { task }));
    }

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
        let mut http_handler: Arc<dyn wash_runtime::host::http::HostHandler> =
            Arc::clone(&server) as Arc<dyn wash_runtime::host::http::HostHandler>;
        if let Some(handlers) = application_http.as_ref() {
            let platform = Arc::new(wamn_execution_host::platform_http::PlatformHttp::new(
                Arc::clone(&server) as Arc<dyn wash_runtime::host::http::HostHandler>,
                Arc::clone(handlers),
                Arc::clone(&flow_http),
            ));
            http_handler = platform;
        }
        let connections = server.connection_limit();
        probe_state.register(Arc::new(connections.clone()));
        ingress_connections = Some(connections);
        ingress_handler = Some(Arc::clone(&server));
        builder = builder.with_http_handler(http_handler);
    }
    let file_listener = match args.files_addr {
        Some(address) => {
            anyhow::ensure!(
                files.is_some(),
                "--files-addr requires a release host with file routes"
            );
            Some(
                tokio::net::TcpListener::bind(address)
                    .await
                    .with_context(|| format!("bind the native file listener on {address}"))?,
            )
        }
        None => None,
    };
    let probe_listener = match args.probe_addr {
        Some(address) => Some(probes::bind(address).await?),
        None => None,
    };

    let prepared_host = builder.build().context("failed to build native host")?;
    tracing::info!(
        router_delivery = router_driver.is_some(),
        "wamn-host starting (base plugins: wasi:config, wasi:logging, wasi:otel, wamn:postgres, wamn:jetstream, wamn:flow-http-routing)"
    );
    // Whether this host carries a release is the first thing an operator needs from
    // the log: it decides whether the release-conditioned interfaces have a manifest to
    // serve from at all. The binding also has to outlive this point — `release`
    // holds the process's only loaded manifest for as long as `run` is on the
    // stack, which is the whole serving period.
    match release.as_ref() {
        Some(loaded_release) => tracing::info!(
            manifest_digest = %loaded_release.release().manifest_digest,
            "wamn-host loaded its release"
        ),
        None => {
            tracing::info!(
                "wamn-host carries no release; no release-conditioned interface is served"
            );
        }
    }
    let mut file_tasks = tokio::task::JoinSet::new();
    if let Some(listener) = file_listener {
        file_tasks.spawn(wamn_execution_host::file_route::serve(
            listener,
            Arc::clone(files.as_ref().expect("file listener requires file routes")),
            Arc::clone(&flow_http),
        ));
    }
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
                postgres: Arc::clone(&postgres),
                operations: operations.clone(),
                component_artifact_base: args.component_artifact_base.clone(),
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
        resource_leases.shutdown().await;
        let control_result = control.shutdown().await;
        let host_result = cleanup_host.stop().await;
        control_result?;
        host_result
    };
    // The host heartbeats from here on. The preload compiles after it, so a
    // cold compile cache delays the workload and not the heartbeat.
    if files.is_some() {
        let mut stopped = stopping.clone();
        let files = files.clone();
        schedule_tasks.spawn(async move {
            let mut ticks =
                tokio::time::interval(wamn_execution_host::file_collector::COLLECTION_INTERVAL);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut collection = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = stopped.changed() => return,
                    result = collection.join_next(), if !collection.is_empty() => {
                        match result {
                            Some(Ok(Ok(_))) => {},
                            result => tracing::warn!(?result, "host file orphan collection failed"),
                        }
                        continue;
                    }
                    _ = ticks.tick() => {}
                }
                if let Some(files) = &files
                    && collection.is_empty()
                {
                    let files = Arc::clone(files);
                    collection.spawn(async move { files.collect_orphan_files().await });
                }
            }
        });
    }
    let started = preload_release(operations.as_ref()).await;
    if let Err(error) = started {
        let _ = stop_queue.send(true);
        wamn_engine::lifecycle::bounded_cleanup(cleanup_budget, async {
            let _ = workload_drain
                .stop(
                    &native_host,
                    None,
                    Duration::from_secs(args.materializer_drain_bound_seconds),
                )
                .await;
            let had_ingress = ingress_handler.is_some();
            let explicit_stop = match ingress_handler.as_ref() {
                Some(handler) => handler.stop().await.context("stop HTTP admission"),
                None => Ok(()),
            };
            let component_result = workload_drain
                .stop_components(&native_host, async {
                    if let Some(connections) = &ingress_connections {
                        connections.drained().await;
                    }
                })
                .await;
            let native_result = normalize_explicit_ingress_stop(
                native_cleanup.await,
                had_ingress && explicit_stop.is_ok(),
            );
            explicit_stop.and(component_result).and(native_result)
        })
        .await?;

        return Err(error);
    }
    // The host is subscribed and its release is compiled, so readiness can report ready.
    probe_state.started();
    tracing::info!(
        elapsed_ms = %startup_started.elapsed().as_millis(),
        cleanup_budget_secs = cleanup_budget.as_secs(),
        "wamn-host runtime startup completed"
    );
    let result = {
        let queue_serving = async {
            match queue.as_ref() {
                Some(queue) => queue.serve(stopping).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(queue_serving);

        // Polling cleanup requests shutdown. Observe the retained native state instead.
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let stopped = async {
            let queue_failure = async {
                let Some(queue) = queue.as_ref() else {
                    return std::future::pending::<anyhow::Error>().await;
                };
                let liveness = queue.liveness();
                wamn_engine::lifecycle::watch_liveness(
                    &liveness,
                    None,
                    Duration::from_millis(args.queue_lease_ttl_ms).saturating_mul(3),
                )
                .await
            };
            tokio::select! {
                signal = tokio::signal::ctrl_c() => signal.context("receive SIGINT"),
                _ = sigterm.recv() => Ok(()),
                error = queue_failure => Err(error),
                error = wamn_engine::lifecycle::watch_liveness(&liveness, Some(&probe_state), silence_budget) => Err(error),
                () = ingress_stopped(ingress_connections.as_ref()) => Err(anyhow::anyhow!("native HTTP ingress stopped unexpectedly")),
                task = file_tasks.join_next(), if !file_tasks.is_empty() => {
                    Err(anyhow::anyhow!("native file listener stopped unexpectedly: {task:?}"))
                }
                task = probe_tasks.join_next(), if !probe_tasks.is_empty() => {
                    Err(probe_listener_failure(task.as_ref()))
                }
            }
        };
        let queue_enabled = queue.is_some();
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
                .stop(
                    &native_host,
                    None,
                    Duration::from_secs(args.materializer_drain_bound_seconds),
                )
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
            queue_serving.as_mut(),
            queue_enabled,
            stop_queue,
            &probe_state,
            args.drain_delay,
            shutdown_budget(args.drain_bound_seconds),
        )
        .await
    };
    schedule_tasks.shutdown().await;
    if let Some(queue) = &queue {
        queue.revoke();
    }
    drop(queue);
    // Abort and join auxiliary tasks even when the native command task skipped Host::stop.
    file_tasks.abort_all();
    let file_result = wamn_engine::lifecycle::bounded_cleanup(Duration::from_secs(1), async {
        while file_tasks.join_next().await.is_some() {}
        Ok(())
    })
    .await;
    probe_tasks.abort_all();
    let probe_result = wamn_engine::lifecycle::bounded_cleanup(Duration::from_secs(1), async {
        while probe_tasks.join_next().await.is_some() {}
        Ok(())
    })
    .await;
    result.and(file_result).and(probe_result)
}

/// Admit events with the startup release until the host stops.
async fn serve_application_events(
    release: Arc<LoadedRelease>,
    mut stopped: tokio::sync::watch::Receiver<bool>,
    mut admit: impl AsyncFnMut(&Arc<LoadedRelease>),
) {
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = stopped.changed() => return,
            _ = ticks.tick() => {},
        }
        tokio::select! {
            _ = stopped.changed() => return,
            () = admit(&release) => {},
        }
    }
}

/// Pull and compile every component that the release serves.
async fn preload_release(operations: Option<&Arc<OperationHost>>) -> anyhow::Result<()> {
    // SCHEDULE-TIME PRELOAD. prepare_synchronous_release pulls and compiles
    // every digest this release serves and populates the driver's digest-keyed
    // component cache; without it the FIRST request pays a full pull and compile
    // -- measured at 35.8 s in the record retained by commit 1106dc4e6792.
    //
    // It ran only in services/executor before this: the probe was constructed
    // there and nowhere else, so the host that actually serves HTTP routes never
    // preloaded anything. Readiness follows compilation of every released component.
    if let Some(operations) = operations {
        let readiness = wamn_execution_host::RouterReadinessProbe::new(Arc::clone(operations));
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
    Ok(())
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

#[derive(Debug)]
struct ScheduleProbe {
    task: tokio::task::AbortHandle,
}

impl probes::ReadinessCheck for ScheduleProbe {
    fn name(&self) -> &'static str {
        "workflow_schedule"
    }
    fn ready(&self) -> bool {
        !self.task.is_finished()
    }
    fn unrecoverable(&self) -> bool {
        self.task.is_finished()
    }
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
async fn stop_combined_after<I, Q, N>(
    stopped: impl std::future::Future<Output = anyhow::Result<()>>,
    stop_ingress: I,
    had_ingress: bool,
    native_cleanup: N,
    mut queue_serving: std::pin::Pin<&mut Q>,
    queue_enabled: bool,
    stop_queue: tokio::sync::watch::Sender<bool>,
    probes: &ProbeState,
    drain_delay: Duration,
    shutdown_budget: Duration,
) -> anyhow::Result<()>
where
    I: std::future::Future<Output = anyhow::Result<()>>,
    Q: std::future::Future<Output = anyhow::Result<()>>,
    N: std::future::Future<Output = anyhow::Result<()>>,
{
    tokio::pin!(stopped);
    let (result, queue_finished) = tokio::select! {
        result = &mut stopped => (result, false),
        result = &mut queue_serving, if queue_enabled => (
            result.and(Err(anyhow::anyhow!("durable queue loop stopped unexpectedly"))),
            true,
        ),
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
        // The queue keeps leasing the runs pinned to this host's release
        // until none remains, then stops (R20). At the deadline the host stops
        // the queue, runs no further cleanup step and exits. The runs left keep
        // their pin, and another host of the release releases them when their
        // leases lapse.
        let queue_result = if !queue_enabled || queue_finished {
            Ok(())
        } else if let Ok(queue_result) =
            tokio::time::timeout_at(deadline, queue_serving.as_mut()).await
        {
            queue_result
        } else {
            tracing::warn!(
                "the drain bound passed with runs pinned to this release; \
                 stopping without further cleanup"
            );
            return (Ok(()), Ok(()));
        };
        // Native cleanup starts only after the queue stopped.
        let native_result =
            wamn_engine::lifecycle::bounded_cleanup(remaining(), native_cleanup).await;
        (queue_result, native_result)
    };
    let ((queue_result, native_result), ()) = tokio::join!(
        drain_then_cleanup,
        tokio::time::sleep(propagation_delay.min(remaining()))
    );
    let native_result =
        normalize_explicit_ingress_stop(native_result, had_ingress && ingress_result.is_ok());
    let cleanup_result = ingress_result.and(queue_result).and(native_result);
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
#[path = "host/lease_tests.rs"]
mod lease_tests;

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    const RELEASE_BASE: &str = "registry.invalid/wamn/releases";
    const RELEASE_DIGEST: &str =
        "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    #[tokio::test(start_paused = true)]
    async fn event_admission_reuses_the_startup_release_across_ticks() {
        let bytes = anonymous_release(Vec::new()).canonical_bytes();
        let release = Arc::new(
            LoadedRelease::load_canonical_bytes(
                &bytes,
                "startup release",
                ReleaseTarget::new("acme--dev--wms", "dev").with_applications(
                    serde_json::from_str(r#"{"store":{"packages":["cat"]}}"#).unwrap(),
                ),
            )
            .unwrap(),
        );
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let mut admissions = 0;
        serve_application_events(Arc::clone(&release), stopped, async |received| {
            assert!(Arc::ptr_eq(&release, received));
            admissions += 1;
            if admissions == 2 {
                stop.send(true).unwrap();
            }
        })
        .await;
    }

    #[test]
    fn queue_admission_is_hard_for_released_hosts_only() {
        assert!(require_queue_admission(false, None, None).is_ok());
        assert!(require_queue_admission(true, Some("guest"), Some("platform")).is_ok());
        assert!(require_queue_admission(true, Some("guest"), None).is_err());
        assert!(require_queue_admission(true, None, Some("platform")).is_err());
    }

    #[test]
    fn the_host_namespace_is_the_chart_variable_and_never_the_environment() {
        let args = parse([
            "host",
            "--environment",
            "wamn-system",
            "--environment",
            "staging",
        ])
        .unwrap()
        .args;
        assert_eq!(args.environment.as_deref(), Some("staging"));
        assert_eq!(
            host_namespace(Some("wamn-system".to_owned())).as_deref(),
            Some("wamn-system")
        );
        assert_eq!(host_namespace(Some(String::new())), None);
        assert_eq!(host_namespace(None), None);
    }

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
            let error = resolve("events", &overrides).unwrap_err().to_string();
            assert!(error.contains(key), "{error}");
        }
        assert!(
            plugin_bindings(&args, "host:\n  plugins:\n    - id: x\n      image: y\n").is_err()
        );
    }

    #[test]
    fn a_release_environment_argument_replaces_the_chart_namespace() {
        let args = parse([
            "wamn-host",
            "--environment=wamn-system",
            "--drain-delay=5s",
            "--environment=dev",
        ])
        .unwrap()
        .args;
        assert_eq!(args.environment.as_deref(), Some("dev"));
    }

    /// A host that serves a release takes the declared applications beside
    /// its environment, and refuses to start without them.
    #[test]
    fn a_served_release_target_requires_the_declared_applications() {
        let target = |extra: &[&str]| {
            let mut arguments = vec![
                "wamn-host",
                "--org=acme",
                "--project=wms",
                "--environment=dev",
            ];
            arguments.extend_from_slice(extra);
            served_release_target(&parse(arguments).unwrap().args)
        };
        assert!(target(&[]).is_err());
        assert!(target(&["--applications={}"]).is_err());
        assert!(
            target(&[r#"--applications={"a":{"packages":["a"]},"b":{"packages":["b"]}}"#]).is_err()
        );
        let target = target(&[r#"--applications={"wms":{"packages":["wamn_wms"]}}"#])
            .expect("the map parses");
        assert_eq!(
            target.applications["wms"].packages,
            ["wamn_wms".to_owned()].into()
        );
        assert_eq!(target.environment, "dev");
        assert!(
            release_target(
                &parse([
                    "wamn-host",
                    "--org=acme",
                    "--project=wms",
                    "--environment=dev",
                    "--applications=[]"
                ])
                .unwrap()
                .args
            )
            .is_err(),
            "a map that is not the declared applications refuses"
        );
    }

    #[test]
    fn chart_lifecycle_flags_parse_with_native_defaults() {
        let defaults = parse(["wamn-host"]).unwrap().args;
        assert!(defaults.drain_delay.is_zero());
        assert_eq!(defaults.drain_bound_seconds, 300);
        let error = TestCli::try_parse_from(["wamn-host"])
            .expect_err("a host without its drain bound refuses to start");
        assert!(
            error.to_string().contains("--drain-bound-seconds"),
            "{error}"
        );
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

    #[tokio::test]
    async fn cleanup_is_not_polled_before_the_shutdown_trigger() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let cleanup_polled = AtomicBool::new(false);
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let (stop_queue, _stopping) = tokio::sync::watch::channel(false);
        let queue = std::future::pending::<anyhow::Result<()>>();
        tokio::pin!(queue);
        let probes = ProbeState::default();
        let running = stop_combined_after(
            async { stopped.await.context("test shutdown sender") },
            async { Ok(()) },
            false,
            async {
                cleanup_polled.store(true, Ordering::Relaxed);
                Ok(())
            },
            queue.as_mut(),
            false,
            stop_queue,
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
    async fn combined_shutdown_stops_queue_before_polling_both_cleanups() {
        let probes = ProbeState::default();
        let (stop_queue, mut stopping) = tokio::sync::watch::channel(false);
        let native_stopping = stopping.clone();
        let queue = async {
            stopping.changed().await.expect("queue stop sender");
            assert!(*stopping.borrow(), "queue stop precedes cleanup");
            Ok(())
        };
        tokio::pin!(queue);
        stop_combined_after(
            async { Ok(()) },
            async { Ok(()) },
            false,
            async {
                assert!(
                    *native_stopping.borrow(),
                    "HTTP/native cleanup starts after queue admission closes"
                );
                Ok(())
            },
            queue.as_mut(),
            true,
            stop_queue,
            &probes,
            Duration::ZERO,
            Duration::from_secs(1),
        )
        .await
        .expect("combined cleanup");
    }

    /// R20: HTTP admission closes, the queue drains its pinned backlog within
    /// the one shutdown budget, and native cleanup starts only after the queue
    /// stopped.
    #[tokio::test]
    async fn shutdown_closes_admission_drains_the_queue_then_cleans_up() {
        use std::sync::Mutex;
        let order = Mutex::new(Vec::new());
        let probes = ProbeState::default();
        let (stop_queue, mut stopping) = tokio::sync::watch::channel(false);
        let queue = async {
            stopping.changed().await.expect("queue stop sender");
            tokio::time::sleep(Duration::from_millis(300)).await;
            order.lock().unwrap().push("queue");
            Ok(())
        };
        tokio::pin!(queue);
        stop_combined_after(
            async { Ok(()) },
            async {
                order.lock().unwrap().push("ingress");
                Ok(())
            },
            false,
            async {
                order.lock().unwrap().push("native");
                Ok(())
            },
            queue.as_mut(),
            true,
            stop_queue,
            &probes,
            Duration::ZERO,
            Duration::from_secs(5),
        )
        .await
        .expect("combined cleanup");
        assert_eq!(*order.lock().unwrap(), ["ingress", "queue", "native"]);
    }

    /// R20: one budget bounds the whole sequence. With a queue drain and a
    /// native cleanup each slower than the budget, the host stops the queue at
    /// the deadline, runs no further cleanup step and exits cleanly, at the
    /// deadline and not at the sum of the steps.
    #[tokio::test(start_paused = true)]
    async fn shutdown_sequence_exits_within_one_drain_budget() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let budget = shutdown_budget(30);
        assert_eq!(budget, Duration::from_secs(28));
        let native_started = AtomicBool::new(false);
        let probes = ProbeState::default();
        let (stop_queue, mut stopping) = tokio::sync::watch::channel(false);
        let queue = async {
            stopping.changed().await.expect("queue stop sender");
            tokio::time::sleep(Duration::from_secs(100)).await;
            Ok(())
        };
        tokio::pin!(queue);
        let started = tokio::time::Instant::now();
        stop_combined_after(
            async { Ok(()) },
            async {
                tokio::time::sleep(Duration::from_secs(10)).await;
                Ok(())
            },
            false,
            async {
                native_started.store(true, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_secs(100)).await;
                Ok(())
            },
            queue.as_mut(),
            true,
            stop_queue,
            &probes,
            Duration::from_secs(5),
            budget,
        )
        .await
        .expect("the deadline is a clean exit");
        assert_eq!(started.elapsed(), budget);
        assert!(
            !native_started.load(Ordering::Relaxed),
            "no cleanup step runs after the deadline"
        );
    }

    #[tokio::test]
    async fn failed_queue_is_not_polled_again_during_cleanup() {
        let probes = ProbeState::default();
        let (stop_queue, _stopping) = tokio::sync::watch::channel(false);
        let queue = async { Err(anyhow::anyhow!("queue failed")) };
        tokio::pin!(queue);
        stop_combined_after(
            std::future::pending(),
            async { Ok(()) },
            false,
            async { Ok(()) },
            queue.as_mut(),
            true,
            stop_queue,
            &probes,
            Duration::ZERO,
            Duration::from_secs(1),
        )
        .await
        .expect_err("queue failure remains the host result");
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
            let (stop_queue, _stopping) = tokio::sync::watch::channel(false);
            let queue = std::future::pending::<anyhow::Result<()>>();
            tokio::pin!(queue);
            let running = stop_combined_after(
                async {
                    let task = tasks.join_next().await;
                    assert!(task.as_ref().unwrap().as_ref().unwrap_err().is_cancelled());
                    Err(probe_listener_failure(task.as_ref()))
                },
                async { Ok(()) },
                false,
                async {
                    cleanup_polled.store(true, Ordering::Relaxed);
                    Ok(())
                },
                queue.as_mut(),
                false,
                stop_queue,
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
        let (stop_queue, _stopping) = tokio::sync::watch::channel(false);
        let queue = std::future::pending::<anyhow::Result<()>>();
        tokio::pin!(queue);
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            stop_combined_after(
                async { anyhow::bail!("native ingress stopped") },
                async { Ok(()) },
                false,
                std::future::pending(),
                queue.as_mut(),
                false,
                stop_queue,
                &ProbeState::default(),
                Duration::from_secs(60),
                Duration::ZERO,
            ),
        )
        .await
        .expect("failure must skip the signal-only drain delay");
        assert_eq!(result.unwrap_err().to_string(), "native ingress stopped");
        let (stop_queue, _stopping) = tokio::sync::watch::channel(false);
        let queue = std::future::pending::<anyhow::Result<()>>();
        tokio::pin!(queue);
        let result = stop_combined_after(
            async { Ok(()) },
            async { Ok(()) },
            false,
            async { anyhow::bail!("native cleanup failed") },
            queue.as_mut(),
            false,
            stop_queue,
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
    const ADMINISTRATION: &str = "postgres://administration@h/db";

    /// THE GUEST URL IS NOT A CUT-OVER FAMILY'S CREDENTIAL (`wamn-0h0g.22.31`,
    /// `wamn-0h0g.22.11`).
    ///
    /// Driven through [`host_credentials`], the same call `run` makes, so the
    /// ROUTING is what is pinned rather than a re-statement of it. Every class is
    /// validated, not just the ones that matter: naming the wrong class moves BOTH
    /// the class that gained a cut-over url and the one that lost it, and a
    /// two-class assertion could miss the second half.
    #[test]
    fn the_host_routes_each_url_to_its_own_class() {
        let credentials = host_credentials(
            Some(GUEST.to_owned()),
            Some(PLATFORM.to_owned()),
            Some(ADMITTER.to_owned()),
            Some(MATERIALIZER.to_owned()),
            Some(ADMINISTRATION.to_owned()),
        );
        for class in AuthorityClass::ALL {
            let expected = match class {
                AuthorityClass::ExecutorPlatform => PLATFORM,
                AuthorityClass::CallableHttp => ADMITTER,
                AuthorityClass::EventMaterializer => MATERIALIZER,
                AuthorityClass::Administration => ADMINISTRATION,
                AuthorityClass::GuestSql | AuthorityClass::DynamicSql => GUEST,
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
            None,
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
            None,
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

        let neither = host_credentials(Some(GUEST.to_owned()), None, None, None, None);
        assert_eq!(neither.url(AuthorityClass::ExecutorPlatform), None);
        assert_eq!(neither.url(AuthorityClass::CallableHttp), None);
        assert_eq!(neither.url(AuthorityClass::EventMaterializer), None);
        assert_eq!(
            neither.url(AuthorityClass::GuestSql),
            Some(GUEST),
            "the guest class keeps its own credential either way"
        );

        let materializer_only =
            host_credentials(None, None, None, Some(MATERIALIZER.to_owned()), None);
        assert_eq!(
            materializer_only.url(AuthorityClass::EventMaterializer),
            Some(MATERIALIZER)
        );
        assert_eq!(materializer_only.url(AuthorityClass::GuestSql), None);
    }

    #[test]
    fn a_release_backed_host_takes_exactly_one_registry_credential() {
        let file = ["host", "--registry-auth-file", "/tmp/registry.json"];
        let metadata = ["host", "--registry-token-metadata"];
        let parsed = parse(file).unwrap();
        assert!(matches!(
            RegistryPullCredential::select(
                parsed.args.registry_auth_file.as_deref(),
                parsed.args.registry_token_metadata
            ),
            Ok(RegistryPullCredential::AuthFile(_))
        ));
        let parsed = parse(metadata).unwrap();
        assert!(matches!(
            RegistryPullCredential::select(
                parsed.args.registry_auth_file.as_deref(),
                parsed.args.registry_token_metadata
            ),
            Ok(RegistryPullCredential::TokenMetadata)
        ));

        let both = [
            "host",
            "--registry-auth-file",
            "/tmp/registry.json",
            "--registry-token-metadata",
        ];
        assert!(parse(both).is_err());
        let neither = RegistryPullCredential::select(None, false)
            .err()
            .expect("a host with no registry credential refuses");
        assert_eq!(
            neither.to_string(),
            "a release-backed host requires --registry-auth-file or --registry-token-metadata"
        );
    }

    #[test]
    fn session_configuration_requires_the_complete_public_trust_binding() {
        assert!(parse(["host"]).is_ok());
        for arguments in [
            vec!["host", "--session-issuer", "https://identity.invalid"],
            vec!["host", "--session-jwks-ca", "/ca.pem"],
            vec!["host", "--session-instance-suffix", "k3m9x2p7"],
        ] {
            assert!(parse(arguments).is_err());
        }
        let cli = parse([
            "host",
            "--org",
            "fixture",
            "--project",
            "widgets",
            "--session-issuer",
            "https://identity.invalid",
            "--session-jwks-ca",
            "/nonexistent/session-public-ca.pem",
            "--session-instance-suffix",
            "k3m9x2p7",
        ])
        .expect("complete session trust configuration");
        for database in [
            "wamn-db-other--widgets--dev--k3m9x2p7",
            "wamn-db-fixture--other--dev--k3m9x2p7",
            "wamn-db-fixture--widgets--prod--k3m9x2p7",
            "wamn-db-fixture--widgets--dev--a1b2c3d4",
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
    fn callable_http_configuration_is_not_authority_without_release_demand() {
        assert_eq!(
            demanded_http_admitter_url(false, false, Some(ADMITTER.to_owned())),
            None,
            "an anonymous-only release without connections must not name an ambient credential"
        );
        assert_eq!(
            demanded_http_admitter_url(true, false, Some(ADMITTER.to_owned())).as_deref(),
            Some(ADMITTER),
            "a PAT/session-protected release keeps the credential it requires"
        );
    }

    fn anonymous_release(connections: Vec<wamn_catalog::ComponentConnection>) -> ServingManifest {
        use std::collections::{BTreeMap, BTreeSet};
        use wamn_catalog::{
            AdmittedComponentOperation, AttachmentTarget, AttachmentType, ComponentDescriptor,
            DefinitionHash, OperationType, PackageCoordinate, ServingAttachment, ServingComponent,
            ServingRelease, ServingRoute,
        };

        let mut descriptor = ComponentDescriptor::named("http-request", "0.1", RELEASE_DIGEST);
        descriptor.connections = connections;
        ServingManifest::new(
            ServingRelease {
                packages: BTreeSet::from([PackageCoordinate::new("cat", "1.0.0").unwrap()]),
            },
            BTreeSet::from([ServingComponent {
                runtime_schema: None,
                package_id: "cat".into(),
                descriptor: {
                    let mut descriptor = descriptor;
                    descriptor.operations = BTreeMap::from([(
                        "request".into(),
                        AdmittedComponentOperation {
                            host_transaction: false,
                            input_ports: Vec::new(),
                            output_ports: Vec::new(),
                            parameters: Vec::new(),
                            committed_result_schema: None,
                            registered_operation: None,
                            statements: BTreeMap::new(),
                        },
                    )]);
                    descriptor
                },
                statement_instances: BTreeMap::new(),
            }]),
            BTreeSet::from([ServingRoute {
                package_id: "cat".into(),
                component: "http-request".into(),
                operation: "request".into(),
                type_: OperationType::Command,
                reads: BTreeSet::new(),
                revision: None,
                idempotency: None,
            }]),
            BTreeMap::from([(
                "orders".into(),
                ServingAttachment {
                    type_: AttachmentType::Http,
                    package_id: "cat".into(),
                    target: AttachmentTarget::Route {
                        component: "http-request".into(),
                        operation: "request".into(),
                    },
                    definition_hash: DefinitionHash::parse(RELEASE_DIGEST).unwrap(),
                    definition: serde_json::json!({
                        "id": "orders",
                        "type": "http",
                        "source-id": "public",
                        "route": {"path": "/orders", "method": "POST"}
                    }),
                    auth_policy: serde_json::json!({"modes": ["none"]}),
                    registered_operation: None,
                },
            )]),
        )
        .expect("anonymous release is valid")
    }

    #[test]
    fn an_anonymous_release_with_a_connection_keeps_the_callable_http_credential() {
        let manifest = anonymous_release(vec![wamn_catalog::ComponentConnection {
            store_alias: "upstream".into(),
            requirement_type: wamn_catalog::ComponentConnectionType::Http,
            mode: wamn_catalog::ConnectionMode::Write,
        }]);
        let authenticated_routes = requires_pat_route_authentication(&manifest)
            || requires_session_route_authentication(&manifest);
        assert!(!authenticated_routes);
        assert!(declares_connection_requirement(&manifest));
        assert_eq!(
            demanded_http_admitter_url(
                authenticated_routes,
                declares_connection_requirement(&manifest),
                Some(ADMITTER.to_owned()),
            )
            .as_deref(),
            Some(ADMITTER),
            "an anonymous-only release needs the credential to resolve its connection"
        );
    }

    #[test]
    fn an_anonymous_release_without_connections_drops_the_callable_http_credential() {
        let manifest = anonymous_release(Vec::new());
        let authenticated_routes = requires_pat_route_authentication(&manifest)
            || requires_session_route_authentication(&manifest);
        assert!(!authenticated_routes);
        assert!(!declares_connection_requirement(&manifest));
        assert_eq!(
            demanded_http_admitter_url(
                authenticated_routes,
                declares_connection_requirement(&manifest),
                Some(ADMITTER.to_owned()),
            ),
            None,
            "an anonymous-only release without connections must not name an ambient credential"
        );
    }

    /// The R2 posture's first half: a host given no release pair was never given
    /// a release, so there is nothing to pull and nothing to refuse.
    #[tokio::test]
    async fn a_host_given_no_release_pair_carries_no_release() {
        let release = load_release(
            None,
            None,
            false,
            None,
            false,
            &[],
            Ok(ReleaseTarget::new("project", "test")),
        )
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
            false,
            &[],
            Ok(ReleaseTarget::new("project", "test")),
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
            GuestMemoryMode::Count
        );
        let error = parse(["wamn-host", "--guest-memory-mode=off"]).unwrap_err();
        assert!(error.to_string().contains("--guest-memory-mode"));
        assert_eq!(cli.args.socket_egress, EgressMode::Enforce);
        assert_eq!(
            parse(["wamn-host", "--socket-egress=count"])
                .unwrap()
                .args
                .socket_egress,
            EgressMode::Count
        );
        let error = parse(["wamn-host", "--socket-egress=off"]).unwrap_err();
        assert!(error.to_string().contains("--socket-egress"));
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
                parse(["wamn-host", flag, "1"]).is_err(),
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
            let error = parse(["wamn-host", half[0], half[1]])
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
        let cli = parse([
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
