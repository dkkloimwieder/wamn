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
    RouterDriver, RouterDriverConfig, RouterDriverRequest, WIRING_CACHE_CAPACITY_ENV,
    WiringCacheCapacity, WiringResolution,
};
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{
    ProductionClaimResult, ProductionCompletionResult, ProductionLeaseRenewal,
    ProductionReapResult, ProductionRouterAction, ReleaseIdentity, SessionClaims, WamnPostgres,
    WamnPostgresConfig, production_router_action,
};
use wamn_runtime::release_manifest::ReleaseManifestWeld;

const QUEUE_CLAIM_SCOPE: &str = "wamn-executor-queue";
const DEFAULT_LEASE_TTL_MS: u64 = 30_000;
const PRODUCTION_JANITOR_GRACE_MS: i64 = 3_600_000;
const IDLE_POLL_MS: u64 = 250;

#[derive(Debug, Clone)]
struct QueueScope {
    tenant_id: String,
    catalog_id: String,
    environment: String,
}

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

    /// Visibility timeout for one generation-fenced queue claim.
    #[arg(long, default_value_t = DEFAULT_LEASE_TTL_MS)]
    pub lease_ttl_ms: u64,
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
    let scope = QueueScope {
        tenant_id: release.manifest().release.tenant_id.clone(),
        catalog_id: release.manifest().release.catalog_id.clone(),
        environment: release.manifest().release.environment.clone(),
    };
    let lease_ttl_ms = i64::try_from(args.lease_ttl_ms)
        .ok()
        .filter(|ttl| *ttl > 0)
        .context("--lease-ttl-ms must be a positive signed 64-bit integer")?;
    let mut postgres_config = WamnPostgresConfig::from_env();
    postgres_config.database_url = Some(database_url);
    let postgres = Arc::new(WamnPostgres::new(postgres_config)?);
    postgres.register_pool_metrics();
    postgres.bind_session_claims(
        QUEUE_CLAIM_SCOPE,
        &SessionClaims {
            tenant: scope.tenant_id.clone(),
            project: Some(args.project.clone()),
            schema: args.schema.clone(),
            runner: Some(owner.clone()),
            role: None,
            user_id: None,
            release: Some(ReleaseIdentity {
                release_version: release.release().release_version,
                manifest_digest: release.release().manifest_digest.clone(),
            }),
        },
    )?;
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
        Arc::clone(&postgres),
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
        tenant = %scope.tenant_id,
        catalog_id = %scope.catalog_id,
        environment = %scope.environment,
        release_version = release.release().release_version,
        manifest_digest = %release.release().manifest_digest,
        wiring_cache_capacity = args.wiring_cache_capacity.get().get(),
        lease_ttl_ms,
        "executor router queue driver ready"
    );
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let serving = serve_queue(&driver, &postgres, &scope, lease_ttl_ms);
    tokio::pin!(serving);
    let result = tokio::select! {
        result = &mut serving => result,
        _ = tokio::signal::ctrl_c() => Ok(()),
        _ = sigterm.recv() => Ok(()),
    };
    let snapshot = driver.snapshot();
    tracing::info!(
        cache_hits = snapshot.wiring_cache.hits,
        cache_evictions = snapshot.wiring_cache.evictions,
        "executor router driver stopping"
    );
    postgres.revoke_session_claims(QUEUE_CLAIM_SCOPE);
    ticker.abort();
    result
}

async fn serve_queue(
    driver: &RouterDriver,
    postgres: &WamnPostgres,
    scope: &QueueScope,
    lease_ttl_ms: i64,
) -> anyhow::Result<()> {
    loop {
        match drain_one(driver, postgres, scope, lease_ttl_ms).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(error = %error, "executor queue turn failed; retrying");
            }
        }
        tokio::time::sleep(Duration::from_millis(IDLE_POLL_MS)).await;
    }
}

async fn drain_one(
    driver: &RouterDriver,
    postgres: &WamnPostgres,
    scope: &QueueScope,
    lease_ttl_ms: i64,
) -> anyhow::Result<bool> {
    match postgres
        .reap_one_exhausted_production(QUEUE_CLAIM_SCOPE, PRODUCTION_JANITOR_GRACE_MS)
        .await?
    {
        ProductionReapResult::Reaped { run_id } => {
            tracing::info!(run_id, "executor reaped exhausted queue run");
        }
        ProductionReapResult::Empty | ProductionReapResult::EffectAttempt { .. } => {}
    }

    match postgres
        .claim_next_production(QUEUE_CLAIM_SCOPE, lease_ttl_ms)
        .await?
    {
        ProductionClaimResult::Empty => Ok(false),
        ProductionClaimResult::Terminalized {
            run_id,
            status,
            fail_kind,
        } => {
            tracing::info!(
                run_id,
                status = status.as_sql(),
                fail_kind = fail_kind.as_sql(),
                "executor terminalized queue claim without execution"
            );
            Ok(true)
        }
        ProductionClaimResult::Ready {
            run_id,
            payload,
            lease_generation,
            wiring_id,
            wiring_version,
            caller_attached,
        } => {
            let wiring_version = u32::try_from(wiring_version)
                .context("claimed wiring version is not a positive u32")?;
            let request = RouterDriverRequest {
                tenant_id: scope.tenant_id.clone(),
                catalog_id: scope.catalog_id.clone(),
                environment: scope.environment.clone(),
                wiring_id,
                wiring_version,
                delivery_id: run_id.clone(),
                payload,
                caller_attached,
                resolution: WiringResolution::Frozen,
                role: None,
                user_id: None,
                traceparent: None,
                tracestate: None,
            };
            drive_claim(
                driver,
                postgres,
                &run_id,
                lease_generation,
                lease_ttl_ms,
                caller_attached,
                request,
            )
            .await?;
            Ok(true)
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the queue fence and exact router request are independent trusted facts"
)]
async fn drive_claim(
    driver: &RouterDriver,
    postgres: &WamnPostgres,
    run_id: &str,
    lease_generation: i64,
    lease_ttl_ms: i64,
    caller_attached: bool,
    request: RouterDriverRequest,
) -> anyhow::Result<()> {
    let execute = driver.execute(request);
    tokio::pin!(execute);
    let renew_every = Duration::from_millis(
        u64::try_from(lease_ttl_ms)
            .expect("validated lease TTL is positive")
            .div_ceil(3),
    );
    let first_renewal = tokio::time::Instant::now() + renew_every;
    let mut heartbeat = tokio::time::interval_at(first_renewal, renew_every);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let delivery = loop {
        tokio::select! {
            delivery = &mut execute => break delivery?,
            _ = heartbeat.tick() => {
                match postgres
                    .renew_production_lease(
                        QUEUE_CLAIM_SCOPE,
                        run_id,
                        lease_generation,
                        lease_ttl_ms,
                    )
                    .await?
                {
                    ProductionLeaseRenewal::Renewed => {}
                    ProductionLeaseRenewal::FenceLost => {
                        tracing::warn!(run_id, lease_generation, "executor queue fence lost");
                        return Ok(());
                    }
                }
            }
        }
    };

    match production_router_action(&delivery.outcome, caller_attached)? {
        ProductionRouterAction::Complete(completion) => {
            let result = postgres
                .complete_production(QUEUE_CLAIM_SCOPE, run_id, lease_generation, &completion)
                .await?;
            match result {
                ProductionCompletionResult::Terminalized
                | ProductionCompletionResult::AlreadyTerminal(_) => {
                    tracing::info!(run_id, ?result, "executor completed queue run");
                }
                ProductionCompletionResult::FenceLost | ProductionCompletionResult::NotFound => {
                    tracing::warn!(run_id, ?result, "executor completion did not own queue run");
                }
            }
        }
        ProductionRouterAction::Emit { dedup_id, .. } => {
            tracing::warn!(
                run_id,
                dedup_id,
                "router emit awaits the wamn-0h0g.19.8 publisher; lease left for redelivery"
            );
        }
        ProductionRouterAction::Cancelled => {
            tracing::info!(
                run_id,
                "router delivery cancelled; lease left for redelivery"
            );
        }
    }
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
