//! `wamn dev up`: the product command stands up its own environment.
//!
//! `[WAMN-DEV-LIVE]` tested the twelve-stage loop long before anyone could
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

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;

use super::environment::{DevEnvironmentInputs, connect, provision, write_dev_config};

/// The operator credential's file, written into `--root` by
/// [`super::environment::provision_route`]. Named here so the summary can
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
    // Settled before a single credential is minted: an environment is
    // expensive to stand up.
    wamn_control_provision::validate_platform_domain(&args.platform_domain)?;

    std::fs::create_dir_all(&args.root)
        .with_context(|| format!("create the environment directory {}", args.root.display()))?;
    // Minted PATs and credential URLs land here, so the directory is the wall.
    std::fs::set_permissions(&args.root, Permissions::from_mode(0o700))
        .with_context(|| format!("restrict {} to its owner", args.root.display()))?;

    let mut package_sources = Vec::with_capacity(args.packages.len());
    for package in &args.packages {
        package_sources.push(
            package
                .canonicalize()
                .with_context(|| format!("resolve package source {}", package.display()))?,
        );
    }
    let local_artifacts = super::config::LocalArtifacts {
        directory: args.root.canonicalize()?.join("local-artifacts"),
        bindings: args
            .local_bindings
            .as_ref()
            .map(|path| path.canonicalize())
            .transpose()?,
        flow_http_component: args.flow_http_component.canonicalize().with_context(|| {
            format!(
                "resolve local flow-http component {}",
                args.flow_http_component.display()
            )
        })?,
    };
    let inputs = DevEnvironmentInputs {
        local_artifacts,
        host_binary: args.host_binary,
        nats_url: args.nats_url,
        event_nats_url: args.event_nats_url,
        event_nats_username: args.event_nats_username,
        event_nats_password_file: args.event_nats_password_file,
        stream_replicas: args.stream_replicas,
        dup_window_secs: args.dup_window_secs,
        tempo_query_url: args.tempo_query_url,
        otel_exporter_otlp_endpoint: args.otel_exporter_otlp_endpoint,
        route_host: args.route_host,
        platform_domain: args.platform_domain,
        package_sources,
    };

    let broker_options = wamn_control::event_streams::connection_options(
        &args.event_provisioning_username,
        &args.event_provisioning_password_file,
    )?;
    let broker = async_nats::jetstream::new(
        broker_options
            .connect(&inputs.event_nats_url)
            .await
            .context("connect event provisioning credential")?,
    );
    let (admin, admin_task) = connect(&args.system_database_url).await?;
    let environment = provision(
        &args.system_database_url,
        admin.as_ref(),
        &args.root,
        &inputs.platform_domain,
    )
    .await?;
    let event_scope = wamn_control_registry::Triple::new(
        &environment.identity.org,
        &environment.identity.project,
        environment.identity.environment.clone(),
    );
    wamn_control::event_streams::provision(
        &broker,
        &event_scope,
        inputs.stream_replicas,
        std::time::Duration::from_secs(inputs.dup_window_secs),
        &[],
    )
    .await?;
    let config = write_dev_config(
        &args.root,
        &args.system_database_url,
        &environment.template,
        &environment.route,
        &environment.credentials,
        &inputs,
        &environment.identity,
    )?;

    let overlay = args
        .overlay_root
        .as_deref()
        .map(|root| format!(" --overlay-root {}", root.display()))
        .unwrap_or_default();
    println!("environment ready");
    println!("  config: {}", config.display());
    // The operator token is also in dev.json for generated client launch.
    // The local Gate uses its separate management-author token.
    println!(
        "  pat:    {} (operator route-caller PAT, at .stringData.token)",
        args.root.join(ROUTE_CALLER_PAT_FILE).display()
    );
    println!();
    println!("run the loop from the repository root, in another terminal:");
    println!("  wamn dev --config {}{overlay} --tui", config.display());
    admin_task.abort();
    Ok(())
}
