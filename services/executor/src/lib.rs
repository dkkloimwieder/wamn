//! Production composition for the flow serving executor.
//!
//! This service leaf selects only production credentials, clock, randomness,
//! egress, and database adapters. Deterministic scenario capabilities live in
//! the separate `wamn-scenario-worker` artifact.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Args;
use tokio::sync::watch;
use wash_runtime::host::allowed_hosts::AllowedHost;

use wamn_execution_host::{
    DEFAULT_FLOWRUNNER_PATH, ExecutionHost, ExecutionIdentity, production_capabilities,
};
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{self, WamnPostgres, WamnPostgresConfig};

/// Production executor configuration.
#[derive(Debug, Args)]
pub struct ExecutorArgs {
    /// Path to the compiled flowrunner component.
    #[arg(long, default_value = DEFAULT_FLOWRUNNER_PATH)]
    pub flowrunner: PathBuf,

    /// App database URL. Overrides WAMN_PG_URL and DATABASE_URL.
    #[arg(long)]
    pub database_url: Option<String>,

    /// Tenant claim applied to the execution session.
    #[arg(long, default_value = "default")]
    pub tenant: String,

    /// Execution session search path.
    #[arg(long)]
    pub schema: Option<String>,

    /// Stable, replica-unique lease owner.
    #[arg(long, env = "WAMN_RUNNER")]
    pub runner: Option<String>,

    /// Mounted production credential-vault file.
    #[arg(long, env = "WAMN_CREDENTIALS_FILE")]
    pub credentials_file: Option<PathBuf>,

    /// Project whose credentials this executor may resolve.
    #[arg(long, env = "WAMN_PROJECT", default_value = wamn_postgres::DEFAULT_PROJECT)]
    pub project: String,

    /// Production outbound HTTP allowlist. Empty denies all egress.
    #[arg(
        long = "allowed-hosts",
        env = "WAMN_ALLOWED_HOSTS",
        value_delimiter = ','
    )]
    pub allowed_hosts: Vec<String>,

    /// Lease TTL for a claimed run, in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    pub lease_ttl_ms: u64,

    /// Tightest idle poll interval, in milliseconds.
    #[arg(long, default_value_t = wamn_scheduler::DEFAULT_MIN_INTERVAL_MS as u64)]
    pub min_idle_ms: u64,

    /// Widest idle poll interval, in milliseconds.
    #[arg(long, default_value_t = wamn_scheduler::DEFAULT_MAX_INTERVAL_MS as u64)]
    pub max_idle_ms: u64,

    /// NATS URL for best-effort doorbell wakes.
    #[arg(long, default_value = "nats://localhost:4222")]
    pub nats_url: String,

    /// Optional mTLS material for the doorbell NATS connection.
    #[arg(long)]
    pub nats_tls_ca: Option<PathBuf>,
    #[arg(long)]
    pub nats_tls_cert: Option<PathBuf>,
    #[arg(long)]
    pub nats_tls_key: Option<PathBuf>,
}

fn resolve_owner(arg: Option<String>) -> String {
    arg.filter(|owner| !owner.is_empty())
        .or_else(|| {
            std::env::var("HOSTNAME")
                .ok()
                .filter(|owner| !owner.is_empty())
        })
        .unwrap_or_else(|| "wamn-runner".to_string())
}

/// Run the production serving executor until shutdown.
pub async fn run(args: ExecutorArgs) -> anyhow::Result<()> {
    use wash_runtime::washlet::{NatsConnectionOptions, connect_nats};

    wash_runtime::init_crypto();

    let cadence = wamn_scheduler::Cadence::new(args.min_idle_ms as i64, args.max_idle_ms as i64)
        .context("invalid idle poll cadence (--min-idle-ms / --max-idle-ms)")?;
    let database_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let owner = resolve_owner(args.runner.clone());
    let guest = std::fs::read(&args.flowrunner)
        .with_context(|| format!("read flowrunner component {}", args.flowrunner.display()))?;

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

    let engine = build_engine(&[])?;
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let mut executor = ExecutionHost::instantiate(
        &engine,
        &guest,
        postgres,
        credentials,
        logging,
        ExecutionIdentity {
            owner: &owner,
            tenant: &args.tenant,
            schema: args.schema.as_deref(),
            project: &args.project,
        },
        production_capabilities(allowed_hosts),
        args.lease_ttl_ms,
    )
    .await?;

    let nats_options = NatsConnectionOptions {
        request_timeout: None,
        tls_ca: args.nats_tls_ca.clone(),
        tls_first: false,
        tls_cert: args.nats_tls_cert.clone(),
        tls_key: args.nats_tls_key.clone(),
    };
    let nats = match connect_nats(args.nats_url.clone(), nats_options).await {
        Ok(client) => Some(client),
        Err(error) => {
            tracing::warn!(
                url = %args.nats_url,
                error = %error,
                "executor: no NATS; poll reconciliation remains active"
            );
            None
        }
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(error) => {
                    tracing::warn!(error = %error, "executor: no SIGTERM handler; Ctrl-C only");
                    let _ = tokio::signal::ctrl_c().await;
                    let _ = shutdown_tx.send(true);
                    return;
                }
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
        let _ = shutdown_tx.send(true);
    });

    tracing::info!(
        runner = %owner,
        tenant = %args.tenant,
        schema = args.schema.as_deref().unwrap_or("<default>"),
        lease_ttl_ms = args.lease_ttl_ms,
        "executor up"
    );

    let result = executor.serve(nats, cadence, shutdown_rx).await;
    ticker.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_cli_has_no_scenario_capability_switch() {
        use clap::CommandFactory as _;

        #[derive(clap::Parser)]
        struct TestCli {
            #[command(flatten)]
            args: ExecutorArgs,
        }

        let help = TestCli::command().render_long_help().to_string();
        assert!(!help.contains("scenario"));
        assert!(!help.contains("record"));
        assert!(!help.contains("virtual"));
        assert!(!help.contains("seed"));
    }

    #[test]
    fn owner_prefers_explicit_value() {
        assert_eq!(resolve_owner(Some("replica-7".into())), "replica-7");
    }

    #[test]
    fn manifest_excludes_scenario_runtime() {
        let manifest = include_str!("../Cargo.toml");
        assert!(!manifest.contains("wamn-scenario-runtime"));
        assert!(!manifest.contains("../scenario-worker"));
    }
}
