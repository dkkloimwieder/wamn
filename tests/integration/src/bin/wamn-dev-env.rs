//! Stand up the disposable development environment `wamn dev` runs against.
//!
//! `[WAMN-DEV-LIVE]` proved the twelve-stage loop long before anyone could
//! start it: every value the strict configuration needs was minted inside the
//! proof and thrown away with it. This command runs the same standup module the
//! gate runs, leaves the configuration on disk, and holds the authoring Gate
//! open so the loop an operator starts by hand is the loop the gate proves
//! (wamn-10yt.10.30).
//!
//! Point it only at disposable PostgreSQL 18 and registry services. Standup
//! resets the control store, so it is a fresh start every time.

use std::fs::Permissions;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;
use wamn_proof_integration::dev_environment::{
    DevEnvironmentInputs, connect, provision, start_journey_management_gate, write_dev_config,
};

#[derive(Parser)]
#[command(
    name = "wamn-dev-env",
    about = "Stand up the disposable environment the wamn dev loop runs against"
)]
struct Cli {
    /// Admin URL of the disposable PostgreSQL 18 cluster.
    #[arg(long, env = "WAMN_DEV_ENV_SYSTEM_DATABASE_URL")]
    system_database_url: String,

    /// Directory the emitted Secrets, SQL and `dev.json` are written to.
    #[arg(long, env = "WAMN_DEV_ENV_ROOT")]
    root: PathBuf,

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

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    std::fs::create_dir_all(&cli.root)
        .with_context(|| format!("create the environment directory {}", cli.root.display()))?;
    // Minted PATs and credential URLs land here, so the directory is the wall.
    std::fs::set_permissions(&cli.root, Permissions::from_mode(0o700))
        .with_context(|| format!("restrict {} to its owner", cli.root.display()))?;

    let mut package_sources = Vec::with_capacity(cli.packages.len());
    for package in &cli.packages {
        package_sources.push(
            package
                .canonicalize()
                .with_context(|| format!("resolve package source {}", package.display()))?,
        );
    }
    let inputs = DevEnvironmentInputs {
        host_binary: cli.host_binary,
        nats_url: cli.nats_url,
        tempo_query_url: cli.tempo_query_url,
        otel_exporter_otlp_endpoint: cli.otel_exporter_otlp_endpoint,
        flow_http_workload_image: cli.flow_http_workload_image,
        component_artifact_base: cli.component_artifact_base,
        release_artifact_base: cli.release_artifact_base,
        route_host: cli.route_host,
        registry_auth_file: cli.registry_auth_file,
        package_sources,
    };

    let (admin, admin_task) = connect(&cli.system_database_url).await?;
    let environment = provision(&cli.system_database_url, admin.as_ref(), &cli.root).await?;
    let (gate_bind, gate) = start_journey_management_gate(
        &environment.credentials,
        &environment.verification.credential_url,
        &cli.gate_bind,
    )
    .await?;
    let config = write_dev_config(
        &cli.root,
        &cli.system_database_url,
        &environment.route,
        &environment.credentials,
        &environment.verification,
        &gate_bind,
        &inputs,
        &environment.identity,
    )?;

    let overlay = cli
        .overlay_root
        .as_deref()
        .map(|root| format!(" --overlay-root {}", root.display()))
        .unwrap_or_default();
    println!("environment ready");
    println!("  gate:   http://{gate_bind}/authoring");
    println!("  config: {}", config.display());
    println!();
    println!("run the loop from the repository root, in another terminal:");
    println!("  wamn dev --config {}{overlay} --tui", config.display());
    println!();
    println!("this process holds the Gate; stop it with Ctrl-C when the loop is done");

    let served = gate.await.context("join the authoring Gate")?;
    admin_task.abort();
    served
}
