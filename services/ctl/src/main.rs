//! MVP one-shot control-plane verbs.

use std::str::FromStr as _;

use clap::{Parser, Subcommand};
use wamn_ctl::{
    author_wiring, enable_cdc_project_env, migrate_catalog, print_release_env, promote, provision,
    provision_org, provision_project_env, publish_release, push_component, push_release_manifest,
    reconcile_replica_identity, reconcile_run_plane, terminalize_effect_uncertain,
};

#[derive(Parser)]
#[command(name = "wamn-ctl", version, about)]
struct Cli {
    /// Log level (the chart passes this before the subcommand)
    #[arg(long = "log-level", global = true, default_value = "info")]
    log_level: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Provision a per-project Postgres database + credential on the shared cluster (2.3)
    ProvisionProject(provision::ProvisionProjectArgs),
    /// Render a dedicated org's CNPG Cluster set (one per recovery domain, sized by env policy) + record it in the T1 registry (wamn-q3n.6 / D18)
    ProvisionOrg(provision_org::ProvisionOrgArgs),
    /// Render a per-project-env database (CNPG Database CRD) + privilege step + record it in the T1 registry (wamn-q3n.7)
    ProvisionProjectEnv(provision_project_env::ProvisionProjectEnvArgs),
    /// Overlay CDC capture onto a provisioned project-env: publication + failover slot + replication role/Secret + reader registration (wamn-l5i9.9, D19 v3)
    EnableCdcProjectEnv(enable_cdc_project_env::EnableCdcProjectEnvArgs),
    /// Apply a catalog to a project DB: versioned, forward-only migration + lifecycle + history (2.5)
    MigrateCatalog(migrate_catalog::MigrateCatalogArgs),
    /// Validate and publish exact component bytes, then append their T1 library fact
    PushComponent(push_component::PushComponentArgs),
    /// Submit one authored wiring document as an immutable gated wiring version (wamn-1xb5)
    AuthorWiring(author_wiring::AuthorWiringArgs),
    /// INTERIM: mint one format-2 release from named wirings plus hand-authored attachment and registration documents (a projection replaces the documents once the ruled registration-store move lands)
    ///
    /// PRECONDITION: run `reconcile-run-plane` for this tenant and this `--run-schema` FIRST. This verb reads the tenant's `environment_policies` row before it commits and refuses when the row is absent (`environment-policy-not-converged`) as well as when it names another environment than the release carries (`environment-policy-environment-mismatch`), so publishing into a never-reconciled run plane fails rather than passing unchecked.
    PublishRelease(publish_release::PublishReleaseArgs),
    /// Publish canonical format-2 serving-manifest bytes as an immutable OCI artifact
    PushReleaseManifest(push_release_manifest::PushReleaseManifestArgs),
    /// Print the release lines a pod template carries for one minted release (wamn-duyl)
    PrintReleaseEnv(print_release_env::PrintReleaseEnvArgs),
    /// Promote one verified v2 release into a target environment
    Promote(promote::PromoteArgs),
    /// Detect or repair per-entity REPLICA IDENTITY drift from the catalog's registrations — one-shot, idempotent ALTERs (wamn-l5i9.31)
    ReconcileReplicaIdentity(reconcile_replica_identity::ReconcileReplicaIdentityArgs),
    /// Reconcile a project-env's run-plane schema to deploy/sql — create missing tables, additive ALTERs, outbox-era teardown; idempotent (wamn-1wdq)
    ReconcileRunPlane(reconcile_run_plane::ReconcileRunPlaneArgs),
    /// Terminalize one effect-uncertain run from explicit external evidence.
    TerminalizeEffectUncertain(terminalize_effect_uncertain::TerminalizeEffectUncertainArgs),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let level = tracing::Level::from_str(&cli.log_level)
        .map_err(|_| anyhow::anyhow!("invalid log level: {}", cli.log_level))?;
    // Same shape as the pre-split no-OTEL path of
    // wash_runtime::observability::initialize_observability: stderr fmt layer,
    // RUST_LOG overriding --log-level. The verbs report via stdout; this
    // carries dep diagnostics only.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level.as_str()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match cli.command {
        Command::ProvisionProject(args) => provision::run(args).await,
        Command::ProvisionOrg(args) => provision_org::run(args).await,
        Command::ProvisionProjectEnv(args) => provision_project_env::run(args).await,
        Command::EnableCdcProjectEnv(args) => enable_cdc_project_env::run(args).await,
        Command::MigrateCatalog(args) => migrate_catalog::run(args).await,
        Command::PushComponent(args) => push_component::run(args).await,
        Command::AuthorWiring(args) => author_wiring::run(args).await,
        Command::PublishRelease(args) => publish_release::run(args).await,
        Command::PushReleaseManifest(args) => push_release_manifest::run(args).await,
        Command::PrintReleaseEnv(args) => print_release_env::run(args).await,
        Command::Promote(args) => promote::run(args).await,
        Command::ReconcileReplicaIdentity(args) => reconcile_replica_identity::run(args).await,
        Command::ReconcileRunPlane(args) => reconcile_run_plane::run(args).await,
        Command::TerminalizeEffectUncertain(args) => terminalize_effect_uncertain::run(args).await,
    }
}
