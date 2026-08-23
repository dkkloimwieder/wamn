//! Production construction of the shared router driver for queued delivery.
//!
//! Queue claim/verdict adaptation is owned by `wamn-0h0g.19.6`. This leaf
//! constructs the exact driver direct ingress uses and keeps it live.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use wash_runtime::host::allowed_hosts::AllowedHost;

use wamn_execution_host::{
    RouterDriver, RouterDriverConfig, WIRING_CACHE_CAPACITY_ENV, WiringCacheCapacity,
};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{WamnPostgres, WamnPostgresConfig};
use wamn_runtime::release_manifest::ReleaseManifestWeld;

#[derive(Debug, Args)]
pub struct ExecutorArgs {
    /// App database URL. Overrides WAMN_PG_URL and DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Stable, replica-unique owner prefix for node acquisition claims.
    #[arg(long, env = "WAMN_RUNNER")]
    pub runner: Option<String>,

    /// Mounted production credential-vault file.
    #[arg(long, env = "WAMN_CREDENTIALS_FILE")]
    pub credentials_file: Option<PathBuf>,

    /// Project whose platform pool and node credentials this executor uses.
    #[arg(long, env = "WAMN_PROJECT", default_value = "default")]
    pub project: String,

    /// Optional database search path installed at node checkout.
    #[arg(long, env = "WAMN_SCHEMA")]
    pub schema: Option<String>,

    /// Production outbound ceiling for connection-backed HTTP effects.
    #[arg(long, env = "WAMN_ALLOWED_HOSTS", value_delimiter = ',')]
    pub allowed_hosts: Vec<String>,

    /// Directory containing the canonical format-2 serving manifest.
    #[arg(long, env = "WAMN_RELEASE_MANIFEST_ROOT")]
    pub release_manifest_root: PathBuf,

    /// Explicit registry/repository holding digest-addressed node components.
    #[arg(long, env = "WAMN_COMPONENT_ARTIFACT_BASE")]
    pub component_artifact_base: String,

    /// Permit HTTP only for the explicitly configured in-cluster registry.
    #[arg(long, default_value_t = false)]
    pub allow_insecure_registries: bool,

    /// Maximum resolved wirings retained by the one production router driver.
    #[arg(
        long = "wiring-cache-capacity",
        env = WIRING_CACHE_CAPACITY_ENV,
        default_value_t = WiringCacheCapacity::default()
    )]
    pub wiring_cache_capacity: WiringCacheCapacity,
}

fn resolve_owner(arg: Option<String>) -> String {
    arg.filter(|owner| !owner.is_empty())
        .or_else(|| {
            std::env::var("HOSTNAME")
                .ok()
                .filter(|owner| !owner.is_empty())
        })
        .unwrap_or_else(|| "wamn-executor".to_owned())
}

pub async fn run(args: ExecutorArgs) -> anyhow::Result<()> {
    wash_runtime::init_crypto();

    let database_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let owner = resolve_owner(args.runner.clone());
    let release = Arc::new(
        ReleaseManifestWeld::load_from(&args.release_manifest_root).map_err(|error| {
            anyhow::anyhow!(
                "serving release manifest under {} is unusable ({:?}): {error}",
                args.release_manifest_root.display(),
                error.kind()
            )
        })?,
    );
    let mut postgres_config = WamnPostgresConfig::from_env();
    postgres_config.database_url = Some(database_url);
    let postgres = Arc::new(WamnPostgres::new(postgres_config)?);
    postgres.register_pool_metrics();
    let credentials = Arc::new(match &args.credentials_file {
        Some(path) => WamnCredentials::from_file(path)?,
        None => WamnCredentials::empty(),
    });
    let logging = Arc::new(WamnLogging::from_env().context("wamn:logging plugin init")?);
    let allowed_hosts: Arc<[AllowedHost]> = args
        .allowed_hosts
        .iter()
        .map(|value| value.parse::<AllowedHost>())
        .collect::<Result<Vec<_>, _>>()
        .context("parse --allowed-hosts")?
        .into();
    let source = ComponentArtifactSource::new(ComponentArtifactSourceConfig::new(
        &args.component_artifact_base,
        args.allow_insecure_registries,
        Duration::from_secs(30),
    )?);
    let engine = Arc::new(build_engine(&[])?);
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let driver = RouterDriver::new(
        engine,
        postgres,
        credentials,
        logging,
        allowed_hosts,
        Arc::clone(&release),
        source,
        RouterDriverConfig {
            owner_prefix: owner.clone(),
            project: args.project.clone(),
            schema: args.schema.clone(),
            cache_capacity: args.wiring_cache_capacity,
            epoch_tick: DEFAULT_EPOCH_TICK,
        },
    )?;

    tracing::info!(
        runner = %owner,
        release_version = release.release().release_version,
        manifest_digest = %release.release().manifest_digest,
        wiring_cache_capacity = args.wiring_cache_capacity.get().get(),
        "executor router driver ready; queued handoff awaits wamn-0h0g.19.6"
    );
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
    let snapshot = driver.snapshot();
    tracing::info!(
        cache_hits = snapshot.wiring_cache.hits,
        cache_evictions = snapshot.wiring_cache.evictions,
        "executor router driver stopping"
    );
    ticker.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    use super::*;

    #[test]
    fn executor_cli_exposes_the_shared_cache_capacity() {
        #[derive(clap::Parser)]
        struct TestCli {
            #[command(flatten)]
            args: ExecutorArgs,
        }

        let help = TestCli::command().render_long_help().to_string();
        assert!(help.contains("wiring-cache-capacity"));
    }

    #[test]
    fn cache_capacity_rejects_zero_and_defaults_to_1024() {
        assert!("0".parse::<WiringCacheCapacity>().is_err());
        assert_eq!(WiringCacheCapacity::default().get().get(), 1_024);
    }
}
