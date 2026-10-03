//! Arguments and output of the `serve`, `saga-resume` and `saga-abandon`
//! verbs (docs/plan/platform-ui.md §5.5).

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;
use wamn_control::environment_saga;
use wamn_control::serve::{ServeConfig, serve as run_serve};
use wamn_runtime::component_artifact_source::OCI_CA_PATHS_ENV;

use crate::provisioning_verbs::PatIssuerArgs;

/// The inputs of the provisioning worker.
#[derive(Args)]
pub struct ServeArgs {
    /// `wamn_provisioner` URL of the system database, from the Secret
    /// `wamn-provisioner`.
    #[arg(long, env = "WAMN_PROVISIONER_URL", hide_env_values = true)]
    pub system_database_url: String,

    /// Host that every emitted credential URL names, for example
    /// `wamn-pg-rw.platform.svc.cluster.local`.
    #[arg(long)]
    pub db_host: String,

    /// Port that every emitted credential URL names.
    #[arg(long, default_value_t = 5432)]
    pub db_port: u16,

    /// The platform host image, `repository@sha256:<digest>`.
    #[arg(long)]
    pub host_image: String,

    /// The platform gates image, `repository@sha256:<digest>`.
    #[arg(long)]
    pub gates_image: String,

    /// The platform identity image, `repository@sha256:<digest>`.
    #[arg(long)]
    pub identity_image: String,

    /// The event broker.
    #[arg(long)]
    pub nats_url: String,

    /// The worker's own provisioning user of the event broker.
    #[arg(long)]
    pub nats_username: String,

    /// File that holds the password of `--nats-username`.
    #[arg(long)]
    pub nats_password_file: PathBuf,

    /// NATS stream copies of each new source stream.
    #[arg(long)]
    pub stream_replicas: usize,

    /// Duplicate detection window of each new source stream, in seconds.
    #[arg(long)]
    pub dup_window_secs: u64,

    /// The web client bucket, `s3://<bucket>[/<prefix>]` or
    /// `gs://<bucket>[/<prefix>]`.
    #[arg(long)]
    pub ui_bucket: String,

    /// `<registry>/<repository>` base of the package artifacts.
    #[arg(long)]
    pub package_artifact_base: String,

    /// `<registry>/<repository>` base of the component artifacts.
    #[arg(long)]
    pub component_artifact_base: String,

    /// `<registry>/<repository>` base of the release manifests.
    #[arg(long)]
    pub release_artifact_base: String,

    /// `.dockerconfigjson` file carrying the registry credential.
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: PathBuf,

    /// Use plain HTTP for exactly the registry of the artifact bases.
    #[arg(long, default_value_t = false)]
    pub insecure_registry: bool,

    /// PEM CA bundle trusted for the registry, on top of the compiled-in
    /// roots. Repeat or comma-delimit. Env `WASH_OCI_CA_PATHS`.
    #[arg(long = "oci-ca-path", env = OCI_CA_PATHS_ENV, value_delimiter = ',')]
    pub oci_ca_paths: Vec<PathBuf>,

    /// The identity service that issues the management-author PAT.
    #[command(flatten)]
    pub pat_issuer: PatIssuerArgs,

    /// The `wamn-scenario-worker` program that gates each wiring.
    #[arg(long)]
    pub scenario_worker: PathBuf,
}

impl std::fmt::Debug for ServeArgs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServeArgs")
            .field("system_database_url", &"[REDACTED]")
            .field("db_host", &self.db_host)
            .field("ui_bucket", &self.ui_bucket)
            .field("pat_issuer", &self.pat_issuer)
            .finish_non_exhaustive()
    }
}

/// Run create-environment sagas until the process ends.
pub async fn serve(args: ServeArgs) -> anyhow::Result<()> {
    run_serve(&ServeConfig {
        system_database_url: args.system_database_url,
        db_host: args.db_host,
        db_port: args.db_port,
        host_image: args.host_image,
        gates_image: args.gates_image,
        identity_image: args.identity_image,
        nats_url: args.nats_url,
        nats_username: args.nats_username,
        nats_password_file: args.nats_password_file,
        stream_replicas: args.stream_replicas,
        dup_window_secs: args.dup_window_secs,
        ui_bucket: args.ui_bucket,
        package_artifact_base: args.package_artifact_base,
        component_artifact_base: args.component_artifact_base,
        release_artifact_base: args.release_artifact_base,
        registry_auth_file: args.registry_auth_file,
        insecure_registry: args.insecure_registry,
        oci_ca_paths: args.oci_ca_paths,
        pat_issuer: args.pat_issuer.into(),
        scenario_worker: args.scenario_worker,
    })
    .await
}

/// One create-environment saga.
#[derive(Args)]
pub struct SagaArgs {
    /// Postgres URL to the system database.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,

    /// The saga id.
    #[arg(long)]
    pub saga: String,
}

impl std::fmt::Debug for SagaArgs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SagaArgs")
            .field("system_database_url", &"[REDACTED]")
            .field("saga", &self.saga)
            .finish()
    }
}

async fn connect(url: &str) -> anyhow::Result<tokio_postgres::Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect to the system database")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

/// Return a failed saga to pending.
pub async fn resume(args: SagaArgs) -> anyhow::Result<()> {
    let mut client = connect(&args.system_database_url).await?;
    environment_saga::saga_resume(&mut client, &args.saga).await?;
    println!("resumed {}", args.saga);
    Ok(())
}

/// End a failed or pending saga as abandoned.
pub async fn abandon(args: SagaArgs) -> anyhow::Result<()> {
    let mut client = connect(&args.system_database_url).await?;
    environment_saga::saga_abandon(&mut client, &args.saga).await?;
    println!("abandoned {}", args.saga);
    Ok(())
}
