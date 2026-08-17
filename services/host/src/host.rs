//! The `host` subcommand: a ClusterHost deployable by the runtime-operator
//! Helm chart. Arg surface mirrors what the chart's runtime deployment
//! template renders for `wash host` (charts/runtime-operator).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use wash_runtime::engine::WasmProposal;
use wash_runtime::host::HostConfig;
use wash_runtime::host::http::{DynamicRouter, Ingress};
use wash_runtime::plugin;
use wash_runtime::washlet::{ClusterHostBuilder, NatsConnectionOptions, connect_nats};

use wamn_runtime::plugins::{WamnFlowInvocation, WamnJetstream, WamnLogging, WamnPostgres};
use wamn_runtime::release_manifest::ReleaseManifestWeld;
use wamn_runtime::{build_engine, spawn_epoch_ticker};

#[derive(Debug, Args)]
pub struct HostArgs {
    /// The host group label to assign to the host
    #[arg(long = "host-group", default_value = "default")]
    pub host_group: String,

    /// NATS URL for control-plane communications
    #[arg(long = "scheduler-nats-url", default_value = "nats://localhost:4222")]
    pub scheduler_nats_url: String,

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

    /// The host name to assign to the host (chart passes the pod IP)
    #[arg(long = "host-name")]
    pub host_name: Option<String>,

    /// Environment advertised in heartbeats (chart passes the pod namespace)
    #[arg(long = "environment", env = "WASMCLOUD_HOST_ENVIRONMENT")]
    pub environment: Option<String>,

    /// Address for the workload HTTP server
    #[arg(long = "http-addr")]
    pub http_addr: Option<SocketAddr>,

    #[arg(long = "tls-cert-path", requires = "tls_key_path")]
    pub tls_cert_path: Option<PathBuf>,

    #[arg(long = "tls-key-path", requires = "tls_cert_path")]
    pub tls_key_path: Option<PathBuf>,

    #[arg(long = "tls-ca-path")]
    pub tls_ca_path: Option<PathBuf>,

    /// Allow insecure (HTTP) OCI registries — needed for the in-cluster dev registry
    #[arg(long = "allow-insecure-registries", default_value_t = false)]
    pub allow_insecure_registries: bool,

    /// The directory to use for caching OCI artifacts
    #[arg(long = "oci-cache-dir")]
    pub oci_cache_dir: Option<PathBuf>,

    /// Extra wasm proposals to enable on the engine (comma-separated)
    #[arg(long = "wasm-proposal", value_delimiter = ',')]
    pub wasm_proposals: Vec<WasmProposal>,

    /// Epoch tick period in milliseconds (0 disables the ticker, so store
    /// epoch deadlines never fire)
    #[arg(long = "epoch-tick-ms", default_value_t = 10)]
    pub epoch_tick_ms: u64,

    /// Directory the digest-named release-manifest ConfigMap is projected into —
    /// normally `/etc/wamn/release-manifest`, the mount path
    /// `wamn_catalog::RELEASE_MANIFEST_MOUNT_PATH` names.
    ///
    /// Absent means this host serves no release; present and unusable means it
    /// refuses to start. See [`load_release`] for why that distinction lives in
    /// this argument's shape rather than in an error.
    ///
    /// Unlike every flag above it, this one is NOT part of the chart's rendered
    /// surface: `wash host` upstream has no release model. It is set on our own
    /// host template, which `wamn-0h0g.15.98` writes.
    #[arg(long = "release-manifest-root", env = "WAMN_RELEASE_MANIFEST_ROOT")]
    pub release_manifest_root: Option<PathBuf>,
}

/// Load and verify this process's release, or record that it carries none.
///
/// This is the wash host's weld construction site: the one place in this process
/// that reads the mounted manifest. flow-http routing (`wamn-0h0g.15.96`) and
/// jetstream delivery gating (`wamn-0h0g.15.95`) take the loaded manifest from
/// here by reference; nobody loads a second copy.
///
/// # The absent-mount posture, and why it is the same rule as the executor's
///
/// `ExecutionHost`'s `load_plan_release` decided this for the flowrunner process,
/// and it is decided identically here because `wamn-0h0g.15.101` requires one
/// rule for both processes rather than one per host. The two absent-mount cases
/// are told apart by the *argument*, never by inspecting a failure:
///
/// - **No root passed.** This host was never given a release: nothing to mount
///   and nothing to refuse. It serves exactly as it did before the release model
///   existed, which is what keeps the gateway, the S2..S6 paths, the gates and the
///   benches running while `wamn-0h0g.15.98` is still mounting things.
/// - **Passed, but the mount is absent, unreadable or non-canonical.** This host
///   was told it serves a release and cannot. Startup fails and the pod never goes
///   ready — the only refusal worth making, since a host serving an unverified
///   manifest would be routing and gating against nothing.
///
/// Those three are the complete mount-time set. There is deliberately no
/// digest-mismatch case here: the weld takes no expected digest and *derives* both
/// identity halves from the bytes it verified (`wamn-0h0g.15.104`), so the name and
/// the content are one fact and cannot disagree. The only digest comparison in the
/// system is at run time, between a run's recorded digest and the pod's carried
/// one, and it belongs to the claim path.
///
/// Encoding it in the argument is forced, not stylistic: `wamn-0h0g.15.104`
/// collapsed the weld's error kinds to two, so "no mount" and "corrupt mount"
/// both arrive as `ManifestUnreadable` and cannot be separated afterwards.
///
/// # Why this host takes one knob where the executor takes four
///
/// The executor also pulls plan BYTES by digest, so it needs a registry base, an
/// insecure-registry flag and a fetch timeout. This host's two readers serve
/// `attachments` and `registrations`, both of which are already inside the
/// verified manifest, so it fetches nothing. The registry knobs would be
/// configuration for a pull that never happens.
fn load_release(manifest_root: Option<&Path>) -> anyhow::Result<Option<ReleaseManifestWeld>> {
    let Some(manifest_root) = manifest_root else {
        return Ok(None);
    };
    let weld = ReleaseManifestWeld::load_from(manifest_root).map_err(|error| {
        anyhow::anyhow!(
            "serving release manifest under {} is unusable ({:?}): {error}",
            manifest_root.display(),
            error.kind()
        )
    })?;
    Ok(Some(weld))
}

pub async fn run(args: HostArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    // THE WELD IS CONSTRUCTED FIRST, and the ordering is load-bearing rather than
    // tidy. Under ruling wamn-0h0g.15.102 the mounted manifest is the SOLE carrier
    // of the (release version, manifest digest) pair, so every consumer takes the
    // pair from this object — including the claim-time recording that
    // wamn-0h0g.15.103 repoints at it. A component that bound before the weld
    // existed would have no pair to record. Building it here, ahead of the NATS
    // connections, the engine and every plugin, makes that ordering impossible to
    // get wrong, and makes a host that cannot verify its release refuse before it
    // opens a socket. The one-site-per-process guard in
    // tests/conformance/src/runtime_inventory.rs pins both facts across both hosts.
    let release = load_release(args.release_manifest_root.as_deref())?;

    let scheduler_nats_client = connect_nats(
        args.scheduler_nats_url.clone(),
        NatsConnectionOptions {
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

    let engine = Arc::new(build_engine(&args.wasm_proposals)?);
    if args.epoch_tick_ms > 0 {
        spawn_epoch_ticker(&engine, Duration::from_millis(args.epoch_tick_ms));
    }
    let postgres = Arc::new(WamnPostgres::from_env().context("wamn:postgres plugin init")?);
    let logging = Arc::new(WamnLogging::from_env().context("wamn:logging plugin init")?);

    let host_config = HostConfig {
        allow_oci_insecure: args.allow_insecure_registries,
        oci_pull_timeout: Some(Duration::from_secs(30)),
        oci_cache_dir: args.oci_cache_dir.clone(),
    };

    let mut builder = ClusterHostBuilder::default()
        .with_engine((*engine).clone())
        .with_host_config(host_config)
        .with_nats_client(Arc::new(scheduler_nats_client))
        .with_host_group(args.host_group.clone())
        .with_plugin(Arc::new(
            plugin::wasi_config::DynamicConfig::builder()
                .copy_environment(true)
                .build(),
        ))?
        // S5: the custom wamn:logging plugin replaces the vendored TracingLogger
        // — it enriches (host-trusted tenant/project + guest flow/run/node),
        // owns a bounded front queue + drop counter, and ships enriched OTel log
        // records to the collector. Both claim wasi:logging/logging, so exactly
        // one may be registered.
        .with_plugin(logging)?
        .with_plugin(Arc::new(plugin::wasi_otel::WasiOtel::default()))?
        // Pool config from DATABASE_URL / WAMN_PG_* env; without a URL the
        // plugin still links and returns connection-unavailable on use.
        .with_plugin(postgres)?
        // l5i9.17: the wamn:jetstream plugin (E10), first bound by the
        // Service-first materializer. Data-plane URL from WAMN_EVT_NATS_URL
        // (absent ⇒ links but returns connection-unavailable, the WAMN_PG_*
        // posture); the doorbell rings on the control-plane client above.
        .with_plugin(Arc::new(
            WamnJetstream::from_env().with_doorbell(doorbell_client),
        ))?
        .with_plugin(Arc::new(
            WamnFlowInvocation::from_env().context("wamn:flow-invocation plugin init")?,
        ))?;

    if let Some(host_name) = &args.host_name {
        builder = builder.with_host_name(host_name);
    }
    if let Some(environment) = &args.environment {
        builder = builder.with_environment(environment);
    }

    if let Some(addr) = args.http_addr {
        let router = DynamicRouter::default();
        let server = if let (Some(cert), Some(key)) = (&args.tls_cert_path, &args.tls_key_path) {
            let mut tls = wash_runtime::host::http::TlsConfig::new(cert, key);
            if let Some(ca) = args.tls_ca_path.as_deref() {
                tls = tls.with_ca(ca);
            }
            Ingress::new_with_tls(router, addr, tls).await?
        } else {
            Ingress::new(router, addr).await?
        };
        builder = builder.with_http_handler(Arc::new(server));
    }

    let cluster_host = builder.build().context("failed to build cluster host")?;
    tracing::info!(
        "wamn-host starting (plugins: wasi:config, wamn:logging, wasi:otel, wamn:postgres, wamn:jetstream, wamn:flow-invocation)"
    );
    // Whether this host carries a release is the first thing an operator needs from
    // the log: it decides whether the release-gated interfaces have a manifest to
    // serve from at all. The binding also has to outlive this point — `release`
    // holds the process's only loaded manifest for as long as `run` is on the
    // stack, which is the whole serving period.
    match release.as_ref() {
        Some(weld) => tracing::info!(
            release_version = weld.release().release_version,
            manifest_digest = %weld.release().manifest_digest,
            "wamn-host welded to its release"
        ),
        None => {
            tracing::info!("wamn-host carries no release; no release-gated interface is served")
        }
    }
    let cleanup = wash_runtime::washlet::run_cluster_host(cluster_host)
        .await
        .context("failed to start cluster host")?;

    // Kubernetes stops pods with SIGTERM; honor both it and Ctrl-C.
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = sigterm.recv() => {},
    }
    tracing::info!("shutting down wamn-host");
    cleanup.await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The R2 posture's first half: a host given no root was never given a
    /// release, so there is nothing to load and nothing to refuse.
    #[test]
    fn a_host_given_no_root_carries_no_release() {
        let release = load_release(None).expect("no root is not a failure");
        assert!(
            release.is_none(),
            "a host with no release root must carry no release rather than \
             loading one from somewhere else"
        );
    }

    /// The R2 posture's second half, and the reason the distinction cannot be
    /// recovered from the error: this path is absent, which is exactly the fault
    /// an unmounted ConfigMap produces, and it must still be fatal — because the
    /// caller asked for a release.
    #[test]
    fn a_host_given_an_unusable_root_refuses() {
        // Deliberately a path that cannot exist rather than a scratch directory:
        // the fault under test is the absent mount itself, and manufacturing it
        // needs no filesystem.
        let root = Path::new("/nonexistent/wamn-release-manifest-root");
        let error = load_release(Some(root)).expect_err("an unusable root must refuse");
        let message = format!("{error}");
        assert!(
            message.contains("/nonexistent/wamn-release-manifest-root"),
            "the refusal must name the root it could not use: {message}"
        );
        assert!(
            message.contains("ManifestUnreadable"),
            "an absent mount must surface as ManifestUnreadable, the kind \
             wamn-0h0g.15.104 left for it: {message}"
        );
    }
}
