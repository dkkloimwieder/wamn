//! Production composition for the flow serving executor.
//!
//! MVP outcome: crash floor · M0 execution · flow composition.
//!
//! This service leaf selects only production credentials, clock, randomness,
//! egress, and database adapters. It also owns the NATS doorbell, idle cadence,
//! retry, and shutdown lifecycle around the shared execution host. Deterministic
//! scenario capabilities live in the separate `wamn-scenario-worker` artifact.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use futures_util::StreamExt as _;
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram};
use tokio::sync::watch;
use wash_runtime::host::allowed_hosts::AllowedHost;

use wamn_control_registry::identifiers::{
    ExecutionTargetId, doorbell_subject, mvp_execution_target_id,
};
use wamn_execution_host::{
    DEFAULT_FLOWRUNNER_PATH, DriveOutcome, ExecutionHost, ExecutionIdentity, ReleaseSupply,
    production_capabilities,
};
use wamn_runtime::engine::{DEFAULT_EPOCH_TICK, build_engine, spawn_epoch_ticker};
use wamn_runtime::plugins::runner_egress::RunnerEgressPolicy;
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{self, WamnPostgres, WamnPostgresConfig};

/// Repository plan artifacts are published under in the in-cluster dev registry
/// (`deploy/platform/registry.yaml`, Service `registry:5000`). The digest is
/// carried as the tag, so the base itself carries neither tag nor digest.
pub const DEFAULT_PLAN_ARTIFACT_BASE: &str = "registry:5000/wamn/execution-plans";

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

    /// Opaque doorbell routing target. When omitted, the MVP placement adapter
    /// assigns the validated tenant value.
    #[arg(long, env = "WAMN_EXECUTION_TARGET_ID")]
    pub execution_target_id: Option<ExecutionTargetId>,

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

    /// Project-environment org for private host-held credential scope.
    #[arg(long, env = "WAMN_ORG")]
    pub org: Option<String>,

    /// Project-environment name for private host-held credential scope.
    #[arg(long, env = "WAMN_ENVIRONMENT")]
    pub environment: Option<String>,

    /// Exact project database for private host-held credential scope.
    #[arg(long, env = "WAMN_DATABASE")]
    pub database: Option<String>,

    /// Production outbound HTTP allowlist. Empty denies all egress.
    #[arg(
        long = "allowed-hosts",
        env = "WAMN_ALLOWED_HOSTS",
        value_delimiter = ','
    )]
    pub allowed_hosts: Vec<String>,

    /// Directory the digest-named release-manifest ConfigMap is projected into.
    ///
    /// Passing it is what makes this process a serving one: the manifest is the
    /// sole carrier of release identity, and every plan this executor supplies is
    /// named by it. Omitted, the executor starts and reports `unavailable` for
    /// every run rather than resolving plans from anywhere else; passed and
    /// unusable, the executor refuses to start.
    #[arg(long, env = "WAMN_RELEASE_MANIFEST_ROOT")]
    pub release_manifest_root: Option<PathBuf>,

    /// `<registry>/<repository>` execution-plan artifacts are published under.
    #[arg(
        long,
        env = "WAMN_PLAN_ARTIFACT_BASE",
        default_value = DEFAULT_PLAN_ARTIFACT_BASE
    )]
    pub plan_artifact_base: String,

    /// Pull plan artifacts over plain HTTP. The dev registry is anonymous plain
    /// HTTP; the wash host reaches the same one with
    /// `--allow-insecure-registries`.
    #[arg(long, env = "WAMN_INSECURE_PLAN_REGISTRY")]
    pub insecure_plan_registry: bool,

    /// Bound on one plan pull, connect and read alike, in milliseconds.
    ///
    /// A pull runs before any effect, so exceeding it releases the run to be
    /// requeued rather than holding a lease against a registry that never answers.
    #[arg(long, default_value_t = 10_000)]
    pub plan_fetch_timeout_ms: u64,

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

fn resolve_execution_target_id(
    configured: Option<ExecutionTargetId>,
    tenant: &str,
) -> anyhow::Result<ExecutionTargetId> {
    match configured {
        Some(target) => Ok(target),
        None => mvp_execution_target_id(tenant)
            .context("derive MVP execution target from tenant configuration"),
    }
}

/// Map a guest or host terminalization to its bounded metric attribute.
fn outcome_label(outcome: DriveOutcome) -> &'static str {
    match outcome {
        DriveOutcome::Guest(0) => "completed",
        DriveOutcome::Guest(1) => "parked",
        DriveOutcome::Guest(_) => "failed",
        DriveOutcome::ClaimTerminalized { fail_kind, .. } => fail_kind.as_sql(),
        DriveOutcome::InfrastructureFailure => "infrastructure-failure",
    }
}

/// Production run-worker instruments and replica identity attributes.
struct RunMetrics {
    executions: Counter<u64>,
    drive_ms: Histogram<f64>,
    tenant: String,
    project: String,
}

impl RunMetrics {
    fn register(tenant: &str, project: &str) -> Self {
        let meter = opentelemetry::global::meter("wamn-run-worker");
        Self {
            executions: meter
                .u64_counter("wamn.run.executions")
                .with_description(
                    "flow-run drives by terminal outcome (completed / parked / failed)",
                )
                .build(),
            drive_ms: meter
                .f64_histogram("wamn.run.drive.duration_ms")
                .with_description(
                    "wall time to claim and handle one run, including host-owned \
                     non-execution terminalization",
                )
                .build(),
            tenant: tenant.to_string(),
            project: project.to_string(),
        }
    }

    fn record_drive(&self, elapsed: Duration, outcome: DriveOutcome) {
        let base = [
            KeyValue::new("wamn.tenant", self.tenant.clone()),
            KeyValue::new("wamn.project", self.project.clone()),
        ];
        self.drive_ms.record(elapsed.as_secs_f64() * 1000.0, &base);
        self.executions.add(
            1,
            &[
                KeyValue::new("wamn.tenant", self.tenant.clone()),
                KeyValue::new("wamn.project", self.project.clone()),
                KeyValue::new("outcome", outcome_label(outcome)),
            ],
        );
    }
}

/// Drain continuously, waking on a doorbell, idle reconciliation, or shutdown.
///
/// Drain failures are non-fatal: the plugin pool can reconnect on the next
/// call, while an in-flight run's lease remains reclaimable by another replica.
async fn serve(
    executor: &mut ExecutionHost,
    nats: Option<async_nats::Client>,
    subject: String,
    cadence: wamn_scheduler::Cadence,
    metrics: &RunMetrics,
    mut shutdown: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let min = cadence.min();
    let mut subscription = match &nats {
        Some(client) => Some(client.subscribe(subject).await?),
        None => None,
    };
    let mut idle = min;
    loop {
        let found_work = match executor
            .drain_observing(|observation| {
                metrics.record_drive(observation.elapsed, observation.outcome);
                tracing::info!(
                    run_id = observation.run_id,
                    outcome = ?observation.outcome,
                    "run-worker: handled a queue run"
                );
            })
            .await
        {
            Ok(report) => {
                if report.claimed > 0 {
                    tracing::info!(
                        claimed = report.claimed,
                        completed = report.completed,
                        parked = report.parked,
                        failed = report.failed,
                        "run-worker: drained"
                    );
                }
                report.found_work()
            }
            Err(error) => {
                if executor.is_disposed() {
                    return Err(error)
                        .context("run-worker execution instance was interrupted and disposed");
                }
                tracing::warn!(
                    error = %error,
                    "run-worker: drain failed (retrying after backoff)"
                );
                false
            }
        };
        idle = cadence.next_interval(idle, found_work);

        tokio::select! {
            hint = async {
                match subscription.as_mut() {
                    Some(subscription) => subscription.next().await,
                    None => std::future::pending().await,
                }
            } => {
                if hint.is_none() {
                    subscription = None;
                    tracing::warn!(
                        "run-worker: doorbell subscription closed; poll-backoff only"
                    );
                } else {
                    idle = min;
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(idle as u64)) => {}
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    return Ok(());
                }
            }
        }
    }
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
    let execution_target_id =
        resolve_execution_target_id(args.execution_target_id.clone(), &args.tenant)?;
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
            org: args.org.as_deref(),
            environment: args.environment.as_deref(),
            database: args.database.as_deref(),
        },
        production_capabilities(allowed_hosts, Arc::new(RunnerEgressPolicy::default())),
        args.release_manifest_root
            .as_deref()
            .map(|manifest_root| ReleaseSupply {
                manifest_root,
                plan_artifact_base: &args.plan_artifact_base,
                insecure_registry: args.insecure_plan_registry,
                fetch_timeout: Duration::from_millis(args.plan_fetch_timeout_ms),
            }),
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
        execution_target_id = %execution_target_id,
        schema = args.schema.as_deref().unwrap_or("<default>"),
        lease_ttl_ms = args.lease_ttl_ms,
        "executor up"
    );

    let metrics = RunMetrics::register(&args.tenant, &args.project);
    let result = serve(
        &mut executor,
        nats,
        doorbell_subject(&execution_target_id),
        cadence,
        &metrics,
        shutdown_rx,
    )
    .await;
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
    fn doorbell_subject_routes_by_target_not_tenant() {
        let target = resolve_execution_target_id(
            Some("worker-b".parse().expect("valid target")),
            "tenant-a",
        )
        .expect("explicit target");
        assert_eq!(doorbell_subject(&target), "wamn.doorbell.worker-b");
    }

    #[test]
    fn omitted_executor_target_uses_the_mvp_adapter() {
        let target = resolve_execution_target_id(None, "tenant-a").expect("tenant-safe target");
        assert_eq!(target.as_str(), "tenant-a");
    }

    #[test]
    fn idle_backoff_resets_on_work_and_expands_while_idle() {
        let cadence = wamn_scheduler::Cadence::new(250, 30_000).unwrap();
        let (min, max) = (cadence.min(), cadence.max());
        assert_eq!(cadence.next_interval(min, true), min);
        let first_idle = cadence.next_interval(min, false);
        let second_idle = cadence.next_interval(first_idle, false);
        assert!(first_idle > min && second_idle > first_idle && second_idle <= max);
        assert_eq!(cadence.next_interval(first_idle, true), min);
    }

    #[test]
    fn outcome_label_maps_guest_and_host_terminalization_to_bounded_buckets() {
        assert_eq!(outcome_label(DriveOutcome::Guest(0)), "completed");
        assert_eq!(outcome_label(DriveOutcome::Guest(1)), "parked");
        assert_eq!(outcome_label(DriveOutcome::Guest(99)), "failed");
        assert_eq!(
            outcome_label(DriveOutcome::InfrastructureFailure),
            "infrastructure-failure"
        );
    }

    #[test]
    fn manifests_keep_lifecycle_dependencies_in_executor() {
        let executor_manifest = include_str!("../Cargo.toml");
        assert!(!executor_manifest.contains("../scenario-worker"));
        for dependency in [
            "async-nats",
            "futures-util",
            "opentelemetry",
            "wamn-control-registry",
            "wamn-scheduler",
        ] {
            assert!(
                executor_manifest.contains(dependency),
                "executor must own {dependency}"
            );
        }

        let host_manifest = include_str!("../../../crates/execution/host/Cargo.toml");
        for dependency in [
            "async-nats",
            "futures-util",
            "opentelemetry",
            "wamn-scheduler",
        ] {
            assert!(
                !host_manifest.contains(dependency),
                "shared execution host must not own {dependency}"
            );
        }
    }
}
