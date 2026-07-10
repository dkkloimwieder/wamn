//! The `host` subcommand: a ClusterHost deployable by the runtime-operator
//! Helm chart. Arg surface mirrors what the chart's runtime deployment
//! template renders for `wash host` (charts/runtime-operator).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use wash_runtime::engine::WasmProposal;
use wash_runtime::host::HostConfig;
use wash_runtime::host::http::{DynamicRouter, HttpServer};
use wash_runtime::plugin;
use wash_runtime::washlet::{ClusterHostBuilder, NatsConnectionOptions, connect_nats};

use crate::engine::build_engine;
use crate::plugins::{WamnLogging, WamnNodeControl, WamnPostgres};

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
    /// compatibility; wamn-host registers no data-plane plugins in S1, so no
    /// connection is opened.
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
}

pub async fn run(args: HostArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

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

    let engine = build_engine(&args.wasm_proposals)?;
    if args.epoch_tick_ms > 0 {
        crate::engine::spawn_epoch_ticker(&engine, Duration::from_millis(args.epoch_tick_ms));
    }

    let host_config = HostConfig {
        allow_oci_insecure: args.allow_insecure_registries,
        oci_pull_timeout: Some(Duration::from_secs(30)),
        oci_cache_dir: args.oci_cache_dir.clone(),
    };

    let mut builder = ClusterHostBuilder::default()
        .with_engine(engine)
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
        .with_plugin(Arc::new(
            WamnLogging::from_env().context("wamn:logging plugin init")?,
        ))?
        .with_plugin(Arc::new(plugin::wasi_otel::WasiOtel::default()))?
        // Pool config from DATABASE_URL / WAMN_PG_* env; without a URL the
        // plugin still links and returns connection-unavailable on use.
        .with_plugin(Arc::new(
            WamnPostgres::from_env().context("wamn:postgres plugin init")?,
        ))?
        .with_plugin(Arc::new(WamnNodeControl))?;

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
            HttpServer::new_with_tls(router, addr, tls).await?
        } else {
            HttpServer::new(router, addr).await?
        };
        builder = builder.with_http_handler(Arc::new(server));
    }

    let cluster_host = builder.build().context("failed to build cluster host")?;
    tracing::info!(
        "wamn-host starting (plugins: wasi:config, wamn:logging, wasi:otel, wamn:postgres, wamn:node/control[stub])"
    );
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
