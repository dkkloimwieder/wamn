//! Production construction of the shared router driver for queued delivery.
//!
//! Queue claim/verdict adaptation is owned by `wamn-0h0g.19.6`. This leaf
//! constructs the exact driver direct ingress uses and keeps it live.

mod readiness;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use tracing::Instrument as _;
use wash_runtime::host::allowed_hosts::AllowedHost;

use wamn_event_wire::Causation;
use wamn_execution_host::{
    CandidateCaseRequest, CandidateExecutionRefusal, CandidateExecutionRefusalKind,
    CandidateWiringTarget, RouterDriver, RouterDriverConfig, RouterDriverRequest,
    RouterReadinessProbe, WIRING_CACHE_CAPACITY_ENV, WiringCacheCapacity, WiringResolution,
};
use wamn_run_state::FailKind;
use wamn_runtime::component_artifact_source::{
    ComponentArtifactSource, ComponentArtifactSourceConfig,
};
use wamn_runtime::engine::{
    DEFAULT_EPOCH_TICK, PoolSizing, build_engine_sized, spawn_epoch_ticker,
};
use wamn_runtime::plugins::wamn_credentials::WamnCredentials;
use wamn_runtime::plugins::wamn_jetstream::{DerivedPublishRequest, WamnJetstream};
use wamn_runtime::plugins::wamn_logging::WamnLogging;
use wamn_runtime::plugins::wamn_postgres::{
    ProductionClaimResult, ProductionCompletionResult, ProductionLeaseRenewal,
    ProductionReapResult, ProductionRouterAction, ReleaseIdentity, SessionClaims, WamnPostgres,
    WamnPostgresConfig, production_router_action, production_router_result_action,
};
use wamn_runtime::release_manifest::ReleaseManifestWeld;
use wamn_runtime::release_manifest_source::ReleaseManifestSource;

const QUEUE_CLAIM_SCOPE: &str = "wamn-executor-queue";
const DEFAULT_LEASE_TTL_MS: u64 = 30_000;
const PRODUCTION_JANITOR_GRACE_MS: i64 = 3_600_000;
const IDLE_POLL_MS: u64 = 250;

#[derive(Debug, Clone)]
struct QueueScope {
    tenant_id: String,
    project: String,
    catalog_id: String,
    environment: String,
}

enum QueueDriverRequest {
    Released(RouterDriverRequest),
    Candidate(CandidateCaseRequest),
}

fn queue_delivery_span(
    scope: &QueueScope,
    run_id: &str,
    wiring_id: &str,
    wiring_version: u32,
) -> tracing::Span {
    tracing::info_span!(
        target: "wamn::router",
        parent: None,
        "wamn.queue.delivery",
        wamn.tenant = %scope.tenant_id,
        wamn.project = %scope.project,
        wamn.catalog_id = %scope.catalog_id,
        wamn.environment = %scope.environment,
        wamn.run_id = %run_id,
        wamn.wiring_id = %wiring_id,
        wamn.wiring_version = wiring_version,
    )
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

    /// Explicit registry/repository holding the release-manifest artifacts.
    ///
    /// The manifest arrives over OCI rather than as a projected ConfigMap: a
    /// mount carries no usable binding between the bytes and the name the
    /// template asked for, while a registry is a third party the digest below
    /// can be proven against (`crates/platform/runtime/src/release_manifest_source.rs`).
    #[arg(long, env = "WAMN_RELEASE_ARTIFACT_BASE")]
    pub release_artifact_base: String,

    /// SHA-256 digest, `sha256:<hex>`, of the one serving manifest this
    /// executor is welded to. Travels in the pod template; the registry's bytes
    /// are refused unless they hash to exactly this.
    #[arg(long, env = "WAMN_RELEASE_MANIFEST_DIGEST")]
    pub release_manifest_digest: String,

    /// Explicit registry/repository holding digest-addressed node components.
    #[arg(long, env = "WAMN_COMPONENT_ARTIFACT_BASE")]
    pub component_artifact_base: String,

    /// Projected `.dockerconfigjson` file for the component registry.
    #[arg(long, env = "WAMN_REGISTRY_AUTH_FILE")]
    pub registry_auth_file: PathBuf,

    /// Permit HTTP only for the explicitly configured in-cluster registry.
    #[arg(long, default_value_t = false)]
    pub allow_insecure_registries: bool,

    /// Extra PEM CA bundles trusted when pulling from OCI registries: for a
    /// registry behind a private or in-cluster CA, which the compiled-in public
    /// roots do not cover.
    ///
    /// Prefer this to `--allow-insecure-registries`, which does not relax
    /// verification but replaces it: that flag switches every registry to plain
    /// HTTP, so credentials travel in the clear and no certificate is checked.
    ///
    /// Spelled exactly as the host's, so one registry posture reads the same on
    /// both deployables (`services/host/src/host.rs`).
    #[arg(long = "oci-ca-path", env = "WASH_OCI_CA_PATHS", value_delimiter = ',')]
    pub oci_ca_paths: Vec<PathBuf>,

    /// Private status-only HTTP listener for the Kubernetes readiness probe.
    #[arg(long, env = "WAMN_READINESS_BIND", default_value = readiness::DEFAULT_BIND)]
    pub readiness_bind: SocketAddr,

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

    /// Pooling-allocator instance slots: the concurrent live instances this
    /// executor admits. Spelled exactly as the host's, so one capacity posture
    /// reads the same on both deployables (`services/host/src/host.rs`).
    #[arg(long = "pool-slots", env = "WAMN_POOL_SLOTS", default_value_t = PoolSizing::default().slots)]
    pub pool_slots: u32,
}

/// Pull, verify and weld this process's one release.
///
/// This is the executor's weld construction site: the single place in this
/// process that turns registry bytes into the (release version, manifest
/// digest) pair every consumer reads. Under ruling `wamn-0h0g.15.102` the
/// manifest is that pair's sole carrier, so a second construction would be a
/// second carrier with nothing reconciling them.
///
/// Refusal is the only outcome besides a welded release: unlike the host, an
/// executor exists to serve a release, so "no release" is not a posture it has.
async fn load_release(
    artifact_base: &str,
    manifest_digest: &str,
    insecure_registry: bool,
    registry_auth_file: &Path,
    ca_paths: &[PathBuf],
) -> anyhow::Result<Arc<ReleaseManifestWeld>> {
    let source = ReleaseManifestSource::new(artifact_base, insecure_registry, registry_auth_file)
        .context("configure the release-manifest registry")?
        .with_ca_paths(ca_paths)
        .context("trust the configured OCI CA bundles for the release pull")?;
    let canonical_bytes = source
        .pull_verified(manifest_digest)
        .await
        .context("pull the serving release manifest")?;
    let origin = format!("{artifact_base}@{manifest_digest}");
    let weld =
        ReleaseManifestWeld::load_canonical_bytes(&canonical_bytes, &origin).map_err(|error| {
            anyhow::anyhow!(
                "serving release manifest {origin} is unusable ({:?}): {error}",
                error.kind()
            )
        })?;
    // Shared by reference-count: the driver and the session-claim scope both
    // outlive `run`'s stack frame, and one allocation stays the process's only
    // manifest.
    Ok(Arc::new(weld))
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

    // Installed once, before anything can pull, and the call that validates the
    // bundles: an executor pointed at an unreadable or unusable CA refuses here
    // rather than starting and rejecting every pull from its registry.
    if !args.oci_ca_paths.is_empty() {
        wash_runtime::oci::set_extra_ca_certificates(&args.oci_ca_paths)
            .context("trust the configured OCI CA bundles")?;
    }

    let database_url = args
        .database_url
        .clone()
        .or_else(|| std::env::var("WAMN_PG_URL").ok())
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("no database url: pass --database-url or set WAMN_PG_URL / DATABASE_URL")?;
    let owner = resolve_owner(args.runner.clone());
    let release = load_release(
        &args.release_artifact_base,
        &args.release_manifest_digest,
        args.allow_insecure_registries,
        &args.registry_auth_file,
        &args.oci_ca_paths,
    )
    .await?;
    let scope = QueueScope {
        tenant_id: release.manifest().release.tenant_id.clone(),
        project: args.project.clone(),
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
    let jetstream = Arc::new(WamnJetstream::from_env());
    jetstream.bind_derived_scope(
        QUEUE_CLAIM_SCOPE,
        &scope.tenant_id,
        &args.project,
        &scope.environment,
    )?;
    let allowed_hosts: Arc<[AllowedHost]> = args
        .allowed_hosts
        .iter()
        .map(|value| value.parse::<AllowedHost>())
        .collect::<Result<Vec<_>, _>>()
        .context("parse --allowed-hosts")?
        .into();
    let source_config = ComponentArtifactSourceConfig::new(
        &args.component_artifact_base,
        args.allow_insecure_registries,
        Duration::from_secs(30),
    )?
    .with_registry_auth_file(&args.registry_auth_file)
    .context("load component registry pull credential")?
    .with_ca_paths(&args.oci_ca_paths)
    .context("trust the configured OCI CA bundles for component pulls")?;
    let source = ComponentArtifactSource::new(source_config);
    let engine = Arc::new(build_engine_sized(
        &[],
        PoolSizing {
            slots: args.pool_slots,
            ..PoolSizing::default()
        },
    )?);
    let ticker = spawn_epoch_ticker(&engine, DEFAULT_EPOCH_TICK);
    let driver = Arc::new(RouterDriver::new(
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
    )?);
    let readiness_probe = Arc::new(RouterReadinessProbe::new(Arc::clone(&driver)));
    let readiness_listener = readiness::bind(args.readiness_bind).await?;

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
    let serving = serve_queue(driver.as_ref(), &postgres, &jetstream, &scope, lease_ttl_ms);
    let readiness = readiness::serve(readiness_listener, readiness_probe);
    tokio::pin!(serving);
    tokio::pin!(readiness);
    let result = tokio::select! {
        result = &mut serving => result,
        result = &mut readiness => result,
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
    jetstream: &WamnJetstream,
    scope: &QueueScope,
    lease_ttl_ms: i64,
) -> anyhow::Result<()> {
    loop {
        match drain_one(driver, postgres, jetstream, scope, lease_ttl_ms).await {
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
    jetstream: &WamnJetstream,
    scope: &QueueScope,
    lease_ttl_ms: i64,
) -> anyhow::Result<bool> {
    match postgres
        .reap_one_exhausted_production(
            QUEUE_CLAIM_SCOPE,
            &scope.catalog_id,
            &scope.environment,
            PRODUCTION_JANITOR_GRACE_MS,
        )
        .await?
    {
        ProductionReapResult::Reaped { run_id } => {
            tracing::info!(run_id, "executor reaped exhausted queue run");
        }
        ProductionReapResult::Empty | ProductionReapResult::EffectAttempt { .. } => {}
    }

    match postgres
        .claim_next_production(
            QUEUE_CLAIM_SCOPE,
            &scope.catalog_id,
            &scope.environment,
            lease_ttl_ms,
        )
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
            router_caller_attached,
            durable_caller_attached,
            candidate,
        } => {
            let wiring_version = u32::try_from(wiring_version)
                .context("claimed wiring version is not a positive u32")?;
            let queue_span = queue_delivery_span(scope, &run_id, &wiring_id, wiring_version);
            let result_only = candidate.is_some();
            let request = match candidate {
                Some(candidate) => QueueDriverRequest::Candidate(CandidateCaseRequest {
                    target: CandidateWiringTarget {
                        tenant_id: scope.tenant_id.clone(),
                        catalog_id: scope.catalog_id.clone(),
                        environment: scope.environment.clone(),
                        catalog_version: u32::try_from(candidate.catalog_version)
                            .context("candidate catalog version is not a positive u32")?,
                        wiring_id,
                        wiring_version,
                        wiring_hash: candidate.wiring_hash,
                        gate_report_id: candidate.gate_report_id,
                    },
                    binding_world: Arc::new(candidate.binding_world),
                    delivery_id: run_id.clone(),
                    payload,
                    traceparent: None,
                    tracestate: None,
                }),
                None => QueueDriverRequest::Released(RouterDriverRequest {
                    tenant_id: scope.tenant_id.clone(),
                    catalog_id: scope.catalog_id.clone(),
                    environment: scope.environment.clone(),
                    wiring_id,
                    wiring_version,
                    delivery_id: run_id.clone(),
                    payload,
                    caller_attached: router_caller_attached,
                    resolution: WiringResolution::Frozen,
                    role: None,
                    user_id: None,
                    traceparent: None,
                    tracestate: None,
                }),
            };
            drive_claim(
                driver,
                postgres,
                jetstream,
                &run_id,
                lease_generation,
                lease_ttl_ms,
                durable_caller_attached,
                result_only,
                request,
            )
            .instrument(queue_span)
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
    jetstream: &WamnJetstream,
    run_id: &str,
    lease_generation: i64,
    lease_ttl_ms: i64,
    durable_caller_attached: bool,
    result_only: bool,
    request: QueueDriverRequest,
) -> anyhow::Result<()> {
    let candidate_coordinate = match &request {
        QueueDriverRequest::Released(_) => None,
        QueueDriverRequest::Candidate(request) => Some((
            request.target.wiring_id.clone(),
            request.target.wiring_version,
        )),
    };
    let execute = async {
        match request {
            QueueDriverRequest::Released(request) => driver.execute(request).await,
            QueueDriverRequest::Candidate(request) => driver.execute_candidate(request).await,
        }
    };
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
            delivery = &mut execute => break delivery,
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

    let delivery = match delivery {
        Ok(delivery) => delivery,
        Err(error) => {
            let Some(refusal) = error.downcast_ref::<CandidateExecutionRefusal>() else {
                return Err(error);
            };
            let Some((wiring_id, wiring_version)) = candidate_coordinate else {
                return Err(error);
            };
            let fail_kind = match refusal.kind() {
                CandidateExecutionRefusalKind::Identity => FailKind::ForeignRevision,
                CandidateExecutionRefusalKind::Definition => FailKind::IncompatibleContract,
                CandidateExecutionRefusalKind::Binding => FailKind::UnboundRequirement,
                CandidateExecutionRefusalKind::Artifact => FailKind::HashInvalidBytes,
            };
            let result = serde_json::json!({
                "error": {
                    "code": refusal.refusal(),
                    "run-id": run_id,
                    "wiring-id": wiring_id,
                    "wiring-version": wiring_version,
                }
            });
            tracing::warn!(
                run_id,
                refusal = refusal.refusal(),
                "candidate queue execution refused deterministic preflight"
            );
            commit_completion(
                postgres,
                run_id,
                lease_generation,
                &wamn_runtime::plugins::wamn_postgres::ProductionCompletion::failed(
                    result, fail_kind, None,
                ),
            )
            .await?;
            return Ok(());
        }
    };

    let action = if result_only {
        production_router_result_action(&delivery.outcome)?
    } else {
        production_router_action(&delivery.outcome, durable_caller_attached)?
    };
    match action {
        ProductionRouterAction::Complete(completion) => {
            commit_completion(postgres, run_id, lease_generation, &completion).await?;
        }
        ProductionRouterAction::Emit {
            event,
            dedup_id,
            entity,
            operation,
        } => {
            let publish = jetstream
                .publish_derived(DerivedPublishRequest {
                    component_id: QUEUE_CLAIM_SCOPE.to_owned(),
                    entity,
                    operation,
                    payload: event.clone(),
                    dedup_id,
                    causation: Causation {
                        run: run_id.to_owned(),
                        root: run_id.to_owned(),
                        depth: 0,
                    },
                })
                .await;
            match publish {
                Ok(ack) => tracing::info!(
                    run_id,
                    stream = ack.stream_name,
                    stream_seq = ack.stream_seq,
                    duplicate = ack.duplicate,
                    "derived event server ACK received before queue completion"
                ),
                Err(error) => {
                    tracing::warn!(
                        run_id,
                        error = %error,
                        error_kind = ?error.kind(),
                        "derived event was not server-acknowledged; lease left for replay"
                    );
                    return Ok(());
                }
            }
            commit_completion(
                postgres,
                run_id,
                lease_generation,
                &wamn_runtime::plugins::wamn_postgres::ProductionCompletion::completed(event, None),
            )
            .await?;
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

async fn commit_completion(
    postgres: &WamnPostgres,
    run_id: &str,
    lease_generation: i64,
    completion: &wamn_runtime::plugins::wamn_postgres::ProductionCompletion,
) -> anyhow::Result<()> {
    let result = postgres
        .complete_production(QUEUE_CLAIM_SCOPE, run_id, lease_generation, completion)
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_sdk::trace::{InMemorySpanExporterBuilder, SdkTracerProvider};
    use tracing_subscriber::layer::SubscriberExt as _;

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
        assert!(help.contains("readiness-bind"));
    }

    #[test]
    fn readiness_bind_has_one_fixed_default_port() {
        #[derive(clap::Parser)]
        struct TestCli {
            #[command(flatten)]
            args: ExecutorArgs,
        }

        let cli = TestCli::try_parse_from([
            "wamn-executor",
            "--release-artifact-base",
            "registry.invalid/wamn/releases",
            "--release-manifest-digest",
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "--component-artifact-base",
            "registry.invalid/wamn/components",
            "--registry-auth-file",
            "/registry/config.json",
        ])
        .expect("complete executor arguments parse");
        assert_eq!(cli.args.readiness_bind.to_string(), readiness::DEFAULT_BIND);
    }

    #[test]
    fn cache_capacity_rejects_zero_and_defaults_to_1024() {
        assert!("0".parse::<WiringCacheCapacity>().is_err());
        assert_eq!(WiringCacheCapacity::default().get().get(), 1_024);
    }

    #[test]
    fn queue_delivery_span_is_an_explicit_host_scoped_root() {
        let exporter = InMemorySpanExporterBuilder::new().build();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("queue-span-test")));
        let _subscriber = tracing::subscriber::set_default(subscriber);
        let scope = QueueScope {
            tenant_id: "tenant-a".to_owned(),
            project: "project-a".to_owned(),
            catalog_id: "orders".to_owned(),
            environment: "prod".to_owned(),
        };

        let ambient = tracing::info_span!("ambient-service-span");
        ambient.in_scope(|| {
            let queue = queue_delivery_span(&scope, "run-9", "route-order", 7);
            queue.in_scope(|| {});
        });
        drop(ambient);
        provider.force_flush().expect("test spans must flush");

        let spans = exporter
            .get_finished_spans()
            .expect("test span exporter must remain readable");
        let queue = spans
            .iter()
            .find(|span| span.name == "wamn.queue.delivery")
            .expect("queue root must be exported");
        assert!(
            queue.parent_span_id == opentelemetry::trace::SpanId::INVALID,
            "the queued automation path must re-root rather than inherit ambient work"
        );
        let attribute = |key: &str| {
            queue
                .attributes
                .iter()
                .find(|attribute| attribute.key.as_str() == key)
                .map(|attribute| attribute.value.to_string())
        };
        assert_eq!(attribute("wamn.tenant").as_deref(), Some("tenant-a"));
        assert_eq!(attribute("wamn.project").as_deref(), Some("project-a"));
        assert_eq!(attribute("wamn.catalog_id").as_deref(), Some("orders"));
        assert_eq!(attribute("wamn.environment").as_deref(), Some("prod"));
        assert_eq!(attribute("wamn.run_id").as_deref(), Some("run-9"));
        assert_eq!(attribute("wamn.wiring_id").as_deref(), Some("route-order"));
        assert_eq!(attribute("wamn.wiring_version").as_deref(), Some("7"));
    }
}
