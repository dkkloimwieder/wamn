//! `wamn dev up`: the product command stands up its own environment.
//!
//! `[WAMN-DEV-LIVE]` tested the loop long before anyone could
//! start it: every value the strict configuration needs was minted inside the
//! test and thrown away with it, so the loop was testable and not startable
//! (wamn-10yt.10.30). This subcommand runs the same standup module the live
//! gates run, writes the strict `dev.json`, and exits.
//!
//! It is not a gate: it emits no test result. Its evidence is that `wamn dev`
//! starts against what it leaves behind.
//!
//! Point it only at disposable PostgreSQL 18 services. Standup resets the
//! control store, so every run is a fresh start.

use std::path::PathBuf;

use clap::Args;

use wamn_control::dev::up::{DevUpRequest, provision_environment};

/// The operator credential's file, written into `--root` by
/// [`wamn_control::dev::environment::provision_route`]. Named here so the summary can
/// point at it: only the path is ever printed, never the token inside it.
const ROUTE_CALLER_PAT_FILE: &str = "route-caller-pat.json";

/// Inputs `wamn dev up` takes to mint one disposable environment.
#[derive(Debug, Args)]
pub struct DevUpArgs {
    /// Admin URL of the disposable PostgreSQL 18 cluster.
    #[arg(long, env = "WAMN_DEV_ENV_SYSTEM_DATABASE_URL")]
    system_database_url: String,

    /// Directory the emitted Secrets, SQL and `dev.json` are written to.
    #[arg(long, env = "WAMN_DEV_ENV_ROOT")]
    root: PathBuf,

    #[arg(long, env = "WAMN_DEV_ENV_NATS_URL")]
    nats_url: String,

    /// Event broker endpoint, separate from the scheduler endpoint.
    #[arg(long, env = "WAMN_EVT_NATS_URL")]
    event_nats_url: String,

    /// Runtime credential for publication and the same environment's tap view.
    #[arg(long, env = "WAMN_EVT_NATS_USERNAME")]
    event_nats_username: String,

    #[arg(long, env = "WAMN_EVT_NATS_PASSWORD_FILE")]
    event_nats_password_file: PathBuf,

    /// Credential that creates the declared event streams.
    #[arg(long, env = "WAMN_DEV_ENV_EVENT_PROVISIONING_USERNAME")]
    event_provisioning_username: String,

    #[arg(long, env = "WAMN_DEV_ENV_EVENT_PROVISIONING_PASSWORD_FILE")]
    event_provisioning_password_file: PathBuf,

    /// Declared NATS stream copies, separate from workload instances.
    #[arg(long, env = "WAMN_EVT_STREAM_REPLICAS")]
    stream_replicas: usize,

    #[arg(long, env = "WAMN_EVT_DUP_WINDOW_SECS")]
    dup_window_secs: u64,

    #[arg(long, env = "WAMN_DEV_ENV_TEMPO_QUERY_URL")]
    tempo_query_url: String,

    #[arg(long, env = "WAMN_DEV_ENV_OTEL_EXPORTER_OTLP_ENDPOINT")]
    otel_exporter_otlp_endpoint: String,

    #[arg(long, env = "WAMN_DEV_ENV_ROUTE_HOST")]
    route_host: String,

    /// Domain of the platform principal emails, `<component>@<platform-domain>`.
    #[arg(long, env = "WAMN_DEV_ENV_PLATFORM_DOMAIN")]
    platform_domain: String,

    /// Built flow-http component the loop loads from its local file.
    #[arg(long, env = "WAMN_DEV_ENV_FLOW_HTTP_COMPONENT")]
    flow_http_component: PathBuf,

    /// Strict local requirement-to-instance selections for components with connections.
    #[arg(long, env = "WAMN_DEV_ENV_LOCAL_BINDINGS")]
    local_bindings: Option<PathBuf>,

    /// The built `wamn-host` the loop supervises.
    #[arg(long, env = "WAMN_DEV_ENV_HOST_BIN")]
    host_binary: PathBuf,

    /// A package source root the loop owns. Repeat for more than one.
    #[arg(long = "package", required = true)]
    packages: Vec<PathBuf>,

    /// Overlay package root, echoed into the printed `wamn dev` command.
    #[arg(long)]
    overlay_root: Option<PathBuf>,
}

/// Stand the environment up and write its configuration.
pub async fn run(args: DevUpArgs) -> anyhow::Result<()> {
    let root = args.root.clone();
    let config = provision_environment(DevUpRequest {
        system_database_url: args.system_database_url,
        root: args.root,
        nats_url: args.nats_url,
        event_nats_url: args.event_nats_url,
        event_nats_username: args.event_nats_username,
        event_nats_password_file: args.event_nats_password_file,
        event_provisioning_username: args.event_provisioning_username,
        event_provisioning_password_file: args.event_provisioning_password_file,
        stream_replicas: args.stream_replicas,
        dup_window_secs: args.dup_window_secs,
        tempo_query_url: args.tempo_query_url,
        otel_exporter_otlp_endpoint: args.otel_exporter_otlp_endpoint,
        route_host: args.route_host,
        platform_domain: args.platform_domain,
        flow_http_component: args.flow_http_component,
        local_bindings: args.local_bindings,
        host_binary: args.host_binary,
        packages: args.packages,
    })
    .await?;

    let overlay = args
        .overlay_root
        .as_deref()
        .map(|root| format!(" --overlay-root {}", root.display()))
        .unwrap_or_default();
    println!("environment ready; identity remains running until wamn dev down");
    println!("  config: {}", config.display());
    // The operator token is also in dev.json for generated client launch.
    // The local Gate uses its separate management-author token.
    println!(
        "  pat:    {} (operator route-caller PAT, at .stringData.token)",
        root.join(ROUTE_CALLER_PAT_FILE).display()
    );
    println!();
    println!("run the loop from the repository root, in another terminal:");
    println!("  wamn dev --config {}{overlay} --tui", config.display());
    Ok(())
}

/// Stop the identity process owned by one development environment.
#[derive(Debug, Args)]
pub struct DevDownArgs {
    #[arg(long, env = "WAMN_DEV_ENV_ROOT")]
    root: PathBuf,
}

/// Leave externally supplied databases and brokers untouched.
pub async fn down(args: DevDownArgs) -> anyhow::Result<()> {
    wamn_control::dev::pat_issuer::stop_environment(&args.root.canonicalize()?).await?;
    println!("development identity stopped");
    Ok(())
}
