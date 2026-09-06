//! `wamn dev up`: the product command stands up its own environment.
//!
//! `[WAMN-DEV-LIVE]` proved the twelve-stage loop long before anyone could
//! start it: every value the strict configuration needs was minted inside the
//! proof and thrown away with it, so the loop was provable and not startable
//! (wamn-10yt.10.30). This subcommand runs the same standup module the live
//! gates run, spawns the authoring Gate as a real child process on a fixed
//! nameable port, writes the strict `dev.json`, and holds until it is stopped.
//!
//! It is not a gate: it emits no receipt. Its evidence is that `wamn dev`
//! starts against what it leaves behind.
//!
//! Point it only at disposable PostgreSQL 18 and registry services. Standup
//! resets the control store, so every run is a fresh start.

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;

use super::environment::{
    DevEnvironmentInputs, connect, gate_listen_address, provision, spawn_journey_management_gate,
    write_dev_config,
};

/// Inputs `wamn dev up` takes to mint one disposable environment.
#[derive(Debug, Args)]
pub struct DevUpArgs {
    /// Admin URL of the disposable PostgreSQL 18 cluster.
    #[arg(long, env = "WAMN_DEV_ENV_SYSTEM_DATABASE_URL")]
    system_database_url: String,

    /// Directory the emitted Secrets, SQL and `dev.json` are written to.
    #[arg(long, env = "WAMN_DEV_ENV_ROOT")]
    root: PathBuf,

    /// The built `wamn-scenario-worker` this command spawns as the Gate.
    #[arg(long, env = "WAMN_DEV_ENV_SCENARIO_WORKER_BIN")]
    scenario_worker_binary: PathBuf,

    /// Address the authoring Gate listens on for the whole session.
    ///
    /// A nameable port, not an ephemeral one: the configuration written here
    /// outlives the process that writes it.
    #[arg(long, default_value = "127.0.0.1:8088")]
    gate_bind: String,

    #[arg(long, env = "WAMN_DEV_ENV_NATS_URL")]
    nats_url: String,

    #[arg(long, env = "WAMN_DEV_ENV_TEMPO_QUERY_URL")]
    tempo_query_url: String,

    #[arg(long, env = "WAMN_DEV_ENV_OTEL_EXPORTER_OTLP_ENDPOINT")]
    otel_exporter_otlp_endpoint: String,

    #[arg(long, env = "WAMN_DEV_ENV_COMPONENT_ARTIFACT_BASE")]
    component_artifact_base: String,

    #[arg(long, env = "WAMN_DEV_ENV_RELEASE_ARTIFACT_BASE")]
    release_artifact_base: String,

    #[arg(long, env = "WAMN_DEV_ENV_REGISTRY_AUTH_FILE")]
    registry_auth_file: PathBuf,

    #[arg(long, env = "WAMN_DEV_ENV_ROUTE_HOST")]
    route_host: String,

    #[arg(long, env = "WAMN_DEV_ENV_FLOW_HTTP_WORKLOAD_IMAGE")]
    flow_http_workload_image: String,

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

/// Stand the environment up, then hold the Gate open until interrupted.
pub async fn run(args: DevUpArgs) -> anyhow::Result<()> {
    // Both settled before a single credential is minted: an environment is
    // expensive to stand up, and a Gate that cannot be started or cannot be
    // named leaves a written configuration nothing can use.
    gate_listen_address(&args.gate_bind)?;
    anyhow::ensure!(
        args.scenario_worker_binary.is_file(),
        "{} does not name a built wamn-scenario-worker binary",
        args.scenario_worker_binary.display()
    );

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
    let inputs = DevEnvironmentInputs {
        host_binary: args.host_binary,
        nats_url: args.nats_url,
        tempo_query_url: args.tempo_query_url,
        otel_exporter_otlp_endpoint: args.otel_exporter_otlp_endpoint,
        flow_http_workload_image: args.flow_http_workload_image,
        component_artifact_base: args.component_artifact_base,
        release_artifact_base: args.release_artifact_base,
        route_host: args.route_host,
        registry_auth_file: args.registry_auth_file,
        package_sources,
    };

    let (admin, admin_task) = connect(&args.system_database_url).await?;
    let environment = provision(&args.system_database_url, admin.as_ref(), &args.root).await?;
    // The Gate admits into the DURABLE project-environment database, the one
    // the host serves from. The verification database is a throwaway this
    // command deletes, so a Gate pointed at it publishes wirings that vanish,
    // and the host's release preload then finds none (wamn-10yt.10.34).
    let mut gate = spawn_journey_management_gate(
        &args.scenario_worker_binary,
        &environment.credentials,
        &environment.credentials.management_admitter,
        &args.gate_bind,
    )
    .await?;
    let config = write_dev_config(
        &args.root,
        &args.system_database_url,
        &environment.route,
        &environment.credentials,
        &environment.verification,
        gate.bind(),
        &inputs,
        &environment.identity,
    )?;

    let overlay = args
        .overlay_root
        .as_deref()
        .map(|root| format!(" --overlay-root {}", root.display()))
        .unwrap_or_default();
    println!("environment ready");
    println!("  gate:   http://{}/authoring", gate.bind());
    println!("  config: {}", config.display());
    println!();
    println!("run the loop from the repository root, in another terminal:");
    println!("  wamn dev --config {}{overlay} --tui", config.display());
    println!();
    println!("this process holds the Gate; stop it with Ctrl-C when the loop is done");

    let bind = gate.bind().to_owned();
    let exited = tokio::select! {
        exited = gate.wait() => Some(exited?),
        interrupted = tokio::signal::ctrl_c() => {
            interrupted.context("wait for the interrupt that stops the environment")?;
            None
        }
    };
    let held = match exited {
        Some(status) => Err(anyhow::anyhow!(
            "the management Gate on {bind} stopped on its own: {status}"
        )),
        None => gate.shutdown().await,
    };
    admin_task.abort();
    held
}
