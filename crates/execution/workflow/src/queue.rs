//! Durable queued delivery driven by the same router as direct ingress.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use tracing::Instrument as _;
use wash_runtime::host::probes::Liveness;

use wamn_engine::release_manifest::LoadedRelease;
use wamn_event_wire::Causation;
use wamn_project_state::PlatformComponent;
use wamn_run_state::{FailKind, RunStore as _};
use wamn_runtime::plugins::wamn_jetstream::{DerivedPublishRequest, WamnJetstream};
use wamn_runtime::plugins::wamn_postgres::{
    ProductionClaimResult, ProductionCompletionResult, ProductionLeaseRenewal,
    ProductionReapResult, ProductionRouterAction, ReleaseIdentity, SessionClaims, WamnPostgres,
    production_router_action, production_router_result_action,
};

use crate::{
    CandidateCaseRequest, CandidateExecutionRefusal, CandidateExecutionRefusalKind,
    CandidateWiringTarget, RouterDriver, RouterDriverRequest, Verdict,
};

const QUEUE_CLAIM_SCOPE: &str = "wamn-executor-queue";
pub const DEFAULT_QUEUE_LEASE_TTL_MS: u64 = 30_000;
const PRODUCTION_JANITOR_GRACE_MS: i64 = 3_600_000;
const IDLE_POLL_MS: u64 = 250;

#[derive(Debug, Clone)]
struct QueueScope {
    tenant_id: String,
    project: String,
    package_ids: Vec<String>,
    environment: String,
}

enum QueueDriverRequest {
    Released(RouterDriverRequest),
    Candidate(CandidateCaseRequest),
}

#[derive(Debug, Clone)]
pub struct QueueServiceConfig {
    pub project: String,
    pub runner: String,
    pub lease_ttl_ms: u64,
}

pub struct QueueService {
    driver: Arc<RouterDriver>,
    postgres: Arc<WamnPostgres>,
    jetstream: Arc<WamnJetstream>,
    scope: QueueScope,
    lease_ttl_ms: i64,
    liveness: Arc<Liveness>,
}

impl std::fmt::Debug for QueueService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueueService")
            .field("scope", &self.scope)
            .field("lease_ttl_ms", &self.lease_ttl_ms)
            .finish_non_exhaustive()
    }
}

impl QueueService {
    pub async fn bind(
        driver: Arc<RouterDriver>,
        postgres: Arc<WamnPostgres>,
        jetstream: Arc<WamnJetstream>,
        release: &LoadedRelease,
        config: QueueServiceConfig,
    ) -> anyhow::Result<Self> {
        let lease_ttl_ms = i64::try_from(config.lease_ttl_ms)
            .ok()
            .filter(|ttl| *ttl > 0)
            .context("queue lease TTL must be a positive signed 64-bit integer")?;
        let scope = QueueScope {
            tenant_id: release.manifest().release.tenant_id.clone(),
            project: config.project.clone(),
            package_ids: release
                .manifest()
                .release
                .packages
                .iter()
                .map(|package| package.package_id().to_owned())
                .collect(),
            environment: release.manifest().release.environment.clone(),
        };
        postgres
            .bind_session_claims(
                QUEUE_CLAIM_SCOPE,
                &SessionClaims {
                    tenant: scope.tenant_id.clone(),
                    project: Some(config.project.clone()),
                    // Queue state belongs to the run plane, never the application schema.
                    schema: Some("wamn_run".to_owned()),
                    runner: Some(config.runner),
                    role: None,
                    user_id: Some(PlatformComponent::Executor.principal_id().to_string()),
                    operation: Some(PlatformComponent::Executor.principal_name().to_owned()),
                    release: Some(ReleaseIdentity {
                        effective_release_id: release.release().effective_release_id,
                        manifest_digest: release.release().manifest_digest.clone(),
                    }),
                },
            )
            .await?;
        jetstream.bind_derived_scope(
            QUEUE_CLAIM_SCOPE,
            &scope.tenant_id,
            &config.project,
            &scope.environment,
        )?;
        let liveness = Liveness::new(Duration::from_millis(config.lease_ttl_ms).saturating_mul(3));
        Ok(Self {
            driver,
            postgres,
            jetstream,
            scope,
            lease_ttl_ms,
            liveness,
        })
    }

    pub fn liveness(&self) -> Arc<Liveness> {
        Arc::clone(&self.liveness)
    }

    pub async fn serve(&self, stopping: tokio::sync::watch::Receiver<bool>) -> anyhow::Result<()> {
        serve_queue(stopping, &self.liveness, async || {
            Box::pin(drain_one(
                &self.driver,
                &self.postgres,
                &self.jetstream,
                &self.scope,
                self.lease_ttl_ms,
                &self.liveness,
            ))
            .await
        })
        .await
    }

    pub fn revoke(&self) {
        self.postgres.revoke_session_claims(QUEUE_CLAIM_SCOPE);
    }
}

fn queue_delivery_span(
    scope: &QueueScope,
    package_id: &str,
    run_id: &str,
    wiring_id: &str,
    wiring_version: u32,
) -> tracing::Span {
    tracing::info_span!(
        target: "wamn::router", parent: None, "wamn.queue.delivery",
        wamn.tenant = %scope.tenant_id, wamn.project = %scope.project,
        wamn.package_id = %package_id, wamn.environment = %scope.environment,
        wamn.run_id = %run_id, wamn.wiring_id = %wiring_id,
        wamn.wiring_version = wiring_version,
    )
}
async fn serve_queue(
    mut stopping: tokio::sync::watch::Receiver<bool>,
    liveness: &Liveness,
    mut turn: impl AsyncFnMut() -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    while !*stopping.borrow() {
        // Real queue progress, including an empty poll or a retry. Long calls
        // also beat after a successful durable lease renewal below.
        liveness.beat();
        match turn().await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(error = %error, "executor queue turn failed; retrying");
            }
        }
        tokio::select! {
            _ = stopping.changed() => return Ok(()),
            () = tokio::time::sleep(Duration::from_millis(IDLE_POLL_MS)) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn shutdown_finishes_current_turn_without_claiming_again() {
        let liveness = Liveness::new(Duration::from_secs(90));
        let turns = AtomicUsize::new(0);
        let release_turn = tokio::sync::Notify::new();
        let (entered, entering) = tokio::sync::oneshot::channel();
        let mut entered = Some(entered);
        let (stop, stopping) = tokio::sync::watch::channel(false);
        let serving = serve_queue(stopping, &liveness, async || {
            turns.fetch_add(1, Ordering::Relaxed);
            entered
                .take()
                .expect("one turn")
                .send(())
                .expect("receiver");
            release_turn.notified().await;
            Ok(true)
        });
        tokio::pin!(serving);
        tokio::select! {
            result = &mut serving => panic!("queue exited during turn: {result:?}"),
            result = entering => result.expect("queue entered"),
        }
        stop.send(true).expect("queue receiver");
        release_turn.notify_one();
        tokio::time::timeout(Duration::from_secs(1), &mut serving)
            .await
            .expect("drain timeout")
            .expect("queue drain");
        assert_eq!(turns.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn shutdown_before_first_turn_never_claims() {
        let liveness = Liveness::new(Duration::from_secs(90));
        let (_stop, stopping) = tokio::sync::watch::channel(true);
        serve_queue(stopping, &liveness, async || {
            panic!("stopped queue claimed")
        })
        .await
        .expect("stopped queue");
        assert_eq!(liveness.silence(), None);
    }
}

#[cfg(test)]
mod automation_live;

async fn drain_one(
    driver: &RouterDriver,
    postgres: &WamnPostgres,
    jetstream: &WamnJetstream,
    scope: &QueueScope,
    lease_ttl_ms: i64,
    liveness: &Liveness,
) -> anyhow::Result<bool> {
    match postgres
        .reap_exhausted(
            QUEUE_CLAIM_SCOPE,
            &scope.package_ids,
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
        .claim_next(
            QUEUE_CLAIM_SCOPE,
            &scope.package_ids,
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
            package_id,
            payload,
            lease_generation,
            wiring_id,
            wiring_version,
            router_caller_attached,
            durable_caller_attached,
            candidate,
            service_principal_id,
        } => {
            let wiring_version = u32::try_from(wiring_version)
                .context("claimed wiring version is not a positive u32")?;
            let queue_span =
                queue_delivery_span(scope, &package_id, &run_id, &wiring_id, wiring_version);
            let result_only = candidate.is_some();
            let caller = match service_principal_id {
                Some(principal) => Some(
                    postgres
                        .queued_service_caller(&scope.project, &scope.tenant_id, &principal)
                        .await?,
                ),
                None => None,
            };
            let request = match candidate {
                Some(candidate) => QueueDriverRequest::Candidate(CandidateCaseRequest {
                    target: CandidateWiringTarget {
                        tenant_id: scope.tenant_id.clone(),
                        package_id: package_id.clone(),
                        environment: scope.environment.clone(),
                        effective_release_id: u32::try_from(candidate.effective_release_id)
                            .context("candidate effective release id is not a positive u32")?,
                        wiring_id,
                        wiring_version,
                        wiring_hash: candidate.wiring_hash,
                    },
                    binding_world: Arc::new(candidate.binding_world),
                    delivery_id: run_id.clone(),
                    payload,
                    traceparent: None,
                    tracestate: None,
                }),
                None => QueueDriverRequest::Released(RouterDriverRequest {
                    tenant_id: scope.tenant_id.clone(),
                    package_id: package_id.clone(),
                    environment: scope.environment.clone(),
                    wiring_id,
                    wiring_version,
                    delivery_id: run_id.clone(),
                    payload,
                    caller_attached: router_caller_attached,
                    caller,
                    traceparent: None,
                    tracestate: None,
                }),
            };
            // Boxed: driving one claim is a large future the queue loop holds.
            Box::pin(
                drive_claim(
                    driver,
                    postgres,
                    jetstream,
                    &package_id,
                    &run_id,
                    lease_generation,
                    lease_ttl_ms,
                    liveness,
                    durable_caller_attached,
                    result_only,
                    request,
                )
                .instrument(queue_span),
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
    jetstream: &WamnJetstream,
    package_id: &str,
    run_id: &str,
    lease_generation: i64,
    lease_ttl_ms: i64,
    liveness: &Liveness,
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
    // Both arms, unlike `candidate_coordinate`, which is deliberately `None` for
    // a released run because it gates a candidate-only refusal. A derived
    // publication names its wiring position on either path, and `request` is
    // consumed by the driver below, so the coordinates are taken here.
    let (wiring_id, wiring_version) = match &request {
        QueueDriverRequest::Released(request) => {
            (request.wiring_id.clone(), request.wiring_version)
        }
        QueueDriverRequest::Candidate(request) => (
            request.target.wiring_id.clone(),
            request.target.wiring_version,
        ),
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
                    .renew(
                        QUEUE_CLAIM_SCOPE,
                        run_id,
                        lease_generation,
                        lease_ttl_ms,
                    )
                    .await?
                {
                    ProductionLeaseRenewal::Renewed => liveness.beat(),
                    ProductionLeaseRenewal::FenceLost => {
                        tracing::warn!(run_id, lease_generation, "executor queue fence lost");
                        return Ok(());
                    }
                }
            }
        }
    };

    let adjustments = match &delivery {
        Ok(delivery) => delivery.deadline_adjustments.as_slice(),
        Err(error) => error
            .downcast_ref::<crate::DeadlineAdjustments>()
            .map_or(&[][..], |adjustments| adjustments.0.as_slice()),
    };
    if !adjustments.is_empty()
        && !postgres
            .record_deadline_adjustments(
                QUEUE_CLAIM_SCOPE,
                run_id,
                lease_generation,
                &serde_json::to_value(adjustments)?,
            )
            .await?
    {
        return Ok(());
    }

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
            // Read off the verdict rather than carried on the action: the node
            // id is a trace coordinate, and `ProductionRouterAction` states what
            // the boundary must DO. Widening it would put a tracing field on the
            // run-store contract, and the outcome that decided the action is
            // still in scope here, so nothing is lost by reading it directly. An
            // empty node id is unreachable — only an Emit verdict yields an Emit
            // action — and records EMPTY rather than panicking if it ever is.
            let node_id = match &delivery.outcome.verdict {
                Some(Verdict::Emit { node_id, .. }) => node_id.clone(),
                _ => String::new(),
            };
            let publish = jetstream
                .publish_derived(DerivedPublishRequest {
                    component_id: QUEUE_CLAIM_SCOPE.to_owned(),
                    package_id: package_id.to_owned(),
                    wiring_id,
                    wiring_version,
                    node_id,
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
        .complete(QUEUE_CLAIM_SCOPE, run_id, lease_generation, completion)
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
