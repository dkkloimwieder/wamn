//! Durable queued delivery driven by the same router as direct ingress.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use tracing::Instrument as _;
use wash_runtime::host::probes::Liveness;

use wamn_engine::native_routes::{OperationRefusal, OperationRefusalType};
use wamn_engine::release_manifest::LoadedRelease;
use wamn_event_wire::Causation;
use wamn_identity_protocol::PlatformComponent;
use wamn_run_state::FailType;
use wamn_runtime::plugins::wamn_jetstream::{DerivedPublishRequest, WamnJetstream};
use wamn_runtime::plugins::wamn_postgres::{
    ProductionCompletionResult, ProductionLeaseRenewal, ProductionLeaseResult,
    ProductionReapResult, ReleaseIdentity, SessionIdentity, WamnPostgres,
};

use crate::resolution::{WorkflowResolutionError, WorkflowResolutionErrorType};
use crate::router_action::{ProductionRouterAction, production_router_action};
use crate::{RouterDriver, WorkflowIdentity, WorkflowRunRequest};

/// The lease target the queue binds; an event admission runs under it too.
pub(crate) const QUEUE_LEASE_INVOCATION: &str = "wamn-executor-queue";
pub const DEFAULT_QUEUE_LEASE_TTL_MS: u64 = 30_000;
const PRODUCTION_JANITOR_GRACE_MS: i64 = 3_600_000;
const IDLE_POLL_MS: u64 = 250;

#[derive(Debug, Clone)]
struct QueueTarget {
    project_env_key: String,
    project: String,
    package_ids: Vec<String>,
    environment: String,
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
    target: QueueTarget,
    lease_ttl_ms: i64,
    liveness: Arc<Liveness>,
}

impl std::fmt::Debug for QueueService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueueService")
            .field("target", &self.target)
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
        let target = QueueTarget {
            project_env_key: release.target().project_env_key.clone(),
            project: config.project.clone(),
            package_ids: release.target().applications.keys().cloned().collect(),
            environment: release.target().environment.clone(),
        };
        postgres
            .bind_session_identity(
                QUEUE_LEASE_INVOCATION,
                &SessionIdentity {
                    project_env_key: target.project_env_key.clone(),
                    project: Some(config.project.clone()),
                    // Queue state belongs to the run plane, never the application schema.
                    schema: Some("wamn_run".to_owned()),
                    runner: Some(config.runner),
                    role: None,
                    user_id: Some(PlatformComponent::Executor.principal_id().to_string()),
                    operation: Some(PlatformComponent::Executor.principal_name().clone()),
                    // The queue serves every application of the release.
                    application: None,
                    release: Some(ReleaseIdentity::from_loaded(release)),
                },
            )
            .await?;
        jetstream.bind_derived_invocation(
            QUEUE_LEASE_INVOCATION,
            &target.project_env_key,
            &config.project,
            &target.environment,
        )?;
        let liveness = Liveness::new(Duration::from_millis(config.lease_ttl_ms).saturating_mul(3));
        Ok(Self {
            driver,
            postgres,
            jetstream,
            target,
            lease_ttl_ms,
            liveness,
        })
    }

    pub fn liveness(&self) -> Arc<Liveness> {
        Arc::clone(&self.liveness)
    }

    /// Serve queue turns until `stopping`, then drain the runs pinned to this
    /// host's release (R20).
    pub async fn serve(&self, stopping: tokio::sync::watch::Receiver<bool>) -> anyhow::Result<()> {
        serve_queue(
            stopping,
            &self.liveness,
            async || {
                Box::pin(drain_one(
                    &self.driver,
                    &self.postgres,
                    &self.jetstream,
                    &self.target,
                    self.lease_ttl_ms,
                    &self.liveness,
                ))
                .await
            },
            async || {
                Ok(self
                    .postgres
                    .pinned_backlog(
                        QUEUE_LEASE_INVOCATION,
                        &self.target.package_ids,
                        &self.target.environment,
                    )
                    .await?)
            },
        )
        .await
    }

    pub fn revoke(&self) {
        self.postgres
            .revoke_session_identity(QUEUE_LEASE_INVOCATION);
    }
}

fn queue_delivery_span(
    target: &QueueTarget,
    package_id: &str,
    run_id: &str,
    wiring_id: &str,
    wiring_version: u32,
) -> tracing::Span {
    tracing::info_span!(
        target: "wamn::router", parent: None, "wamn.queue.delivery",
        wamn.project_env_key = %target.project_env_key, wamn.project = %target.project,
        wamn.package_id = %package_id, wamn.environment = %target.environment,
        wamn.run_id = %run_id, wamn.wiring_id = %wiring_id,
        wamn.wiring_version = wiring_version,
    )
}
/// Take queue turns until `stopping`, then drain: keep taking turns while
/// `pinned_backlog` reports a run pinned to this host's release that is still
/// dispatched or running, and return once it reports none
/// (docs/architecture/execution.md#process-lifecycle). The pod's `terminationGracePeriodSeconds` bounds
/// the drain. Kubernetes owns that clock, so the loop adds no timer.
async fn serve_queue(
    mut stopping: tokio::sync::watch::Receiver<bool>,
    liveness: &Liveness,
    mut turn: impl AsyncFnMut() -> anyhow::Result<bool>,
    mut pinned_backlog: impl AsyncFnMut() -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    loop {
        if *stopping.borrow() {
            match pinned_backlog().await {
                Ok(false) => return Ok(()),
                Ok(true) => {}
                Err(error) => {
                    tracing::warn!(%error, "executor queue backlog read failed; retrying")
                }
            }
        }
        liveness.beat();
        match turn().await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => tracing::warn!(%error, "executor queue turn failed; retrying"),
        }
        if *stopping.borrow() {
            tokio::time::sleep(Duration::from_millis(IDLE_POLL_MS)).await;
        } else {
            tokio::select! {
                _ = stopping.changed() => {},
                () = tokio::time::sleep(Duration::from_millis(IDLE_POLL_MS)) => {},
            }
        }
    }
}

async fn drain_one(
    driver: &RouterDriver,
    postgres: &WamnPostgres,
    jetstream: &WamnJetstream,
    target: &QueueTarget,
    lease_ttl_ms: i64,
    liveness: &Liveness,
) -> anyhow::Result<bool> {
    match postgres
        .reap_exhausted(
            QUEUE_LEASE_INVOCATION,
            &target.package_ids,
            &target.environment,
            PRODUCTION_JANITOR_GRACE_MS,
        )
        .await?
    {
        ProductionReapResult::Reaped { run_id } => {
            tracing::info!(run_id, "executor reaped exhausted queue run");
        }
        ProductionReapResult::Empty => {}
    }

    match postgres
        .lease_next(
            QUEUE_LEASE_INVOCATION,
            &target.package_ids,
            &target.environment,
            lease_ttl_ms,
        )
        .await?
    {
        ProductionLeaseResult::Empty => Ok(false),
        ProductionLeaseResult::Ready {
            run_id,
            package_id,
            payload,
            lease_generation,
            wiring_id,
            wiring_version,

            workflow,
        } => {
            let wiring_version = u32::try_from(wiring_version)
                .context("acquired wiring version is not a positive u32")?;
            let queue_span =
                queue_delivery_span(target, &package_id, &run_id, &wiring_id, wiring_version);
            let workflow = *workflow;
            let request = WorkflowRunRequest {
                project_env_key: target.project_env_key.clone(),
                package_id: package_id.clone(),
                environment: target.environment.clone(),
                wiring_id: wiring_id.clone(),
                wiring_version,
                delivery_id: run_id.clone(),
                payload,
                wiring_hash: workflow.wiring_hash,
                node_digests: workflow.node_digests,
                causation: workflow.causation,
                identity: WorkflowIdentity {
                    principal_id: workflow.execution_principal_id,
                    role: workflow.execution_role,
                },
            };
            // Boxed: driving one lease is a large future the queue loop holds.
            Box::pin(
                drive_lease(
                    driver,
                    postgres,
                    jetstream,
                    &package_id,
                    &run_id,
                    lease_generation,
                    lease_ttl_ms,
                    liveness,
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
async fn drive_lease(
    driver: &RouterDriver,
    postgres: &WamnPostgres,
    jetstream: &WamnJetstream,
    package_id: &str,
    run_id: &str,
    lease_generation: i64,
    lease_ttl_ms: i64,
    liveness: &Liveness,
    request: WorkflowRunRequest,
) -> anyhow::Result<()> {
    let causation = request.causation.clone();
    let (wiring_id, wiring_version) = (request.wiring_id.clone(), request.wiring_version);
    let execute = driver.execute_workflow(request);
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
                        QUEUE_LEASE_INVOCATION,
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

    let (delivery, adjustments) = delivery;
    if !adjustments.is_empty()
        && !postgres
            .record_deadline_adjustments(
                QUEUE_LEASE_INVOCATION,
                run_id,
                lease_generation,
                &serde_json::to_value(&adjustments)?,
            )
            .await?
    {
        return Ok(());
    }

    let delivery = match delivery {
        Ok(delivery) => delivery,
        Err(error) => {
            if let Some((result, fail_type)) = workflow_refusal(&error, run_id) {
                tracing::warn!(run_id, error = %error, "workflow run refused");
                commit_completion(
                    postgres,
                    run_id,
                    lease_generation,
                    &wamn_runtime::plugins::wamn_postgres::ProductionCompletion::failed(
                        result, fail_type,
                    ),
                )
                .await?;
                return Ok(());
            }
            return Err(error);
        }
    };

    let action = production_router_action(&delivery)?;
    match action {
        ProductionRouterAction::Complete(completion) => {
            commit_completion(postgres, run_id, lease_generation, &completion).await?;
        }
        ProductionRouterAction::Emit {
            event,
            dedup_id,
            entity,
            operation,
            node_id,
        } => {
            let publish = jetstream
                .publish_derived(DerivedPublishRequest {
                    component_id: QUEUE_LEASE_INVOCATION.to_owned(),
                    package_id: package_id.to_owned(),
                    wiring_id,
                    wiring_version,
                    node_id,
                    entity,
                    operation,
                    payload: event.clone(),
                    dedup_id,
                    causation: causation.unwrap_or_else(|| Causation {
                        run: run_id.to_owned(),
                        root: run_id.to_owned(),
                        depth: 0,
                    }),
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
                        error_type = ?error.error_type(),
                        "derived event was not server-acknowledged; lease left for replay"
                    );
                    return Ok(());
                }
            }
            commit_completion(
                postgres,
                run_id,
                lease_generation,
                &wamn_runtime::plugins::wamn_postgres::ProductionCompletion::completed(event),
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

/// The failure a workflow run records for a refusal that no retry changes
/// (docs/architecture/execution.md#environment-workflows): a node its principal
/// no longer holds the grant for, or a run that does not resolve against its
/// pinned release. Any other error leaves the lease for redelivery.
fn workflow_refusal(error: &anyhow::Error, run_id: &str) -> Option<(serde_json::Value, FailType)> {
    if let Some(refusal) = error.downcast_ref::<OperationRefusal>() {
        let OperationRefusalType::PermissionDenied = refusal.refusal_type();
        let result = serde_json::json!({"error": {
            "code": "permission_denied",
            "operation": refusal.operation(),
            "run-id": run_id,
        }});
        return Some((result, FailType::Terminal));
    }
    let refusal = error.downcast_ref::<WorkflowResolutionError>()?;
    Some(workflow_resolution_refusal(refusal.error_type(), run_id))
}

fn workflow_resolution_refusal(
    error_type: WorkflowResolutionErrorType,
    run_id: &str,
) -> (serde_json::Value, FailType) {
    let fail_type = match error_type {
        WorkflowResolutionErrorType::NotFound => FailType::UnresolvableName,
        WorkflowResolutionErrorType::HashMismatch => FailType::HashInvalidBytes,
        WorkflowResolutionErrorType::NodeNotInRelease => FailType::ForeignRevision,
        WorkflowResolutionErrorType::Incompatible | WorkflowResolutionErrorType::Definition => {
            FailType::IncompatibleContract
        }
    };
    let result = serde_json::json!({"error": {
        "code": "workflow_resolution_refused",
        "run-id": run_id,
    }});
    (result, fail_type)
}

async fn commit_completion(
    postgres: &WamnPostgres,
    run_id: &str,
    lease_generation: i64,
    completion: &wamn_runtime::plugins::wamn_postgres::ProductionCompletion,
) -> anyhow::Result<()> {
    let result = postgres
        .complete(QUEUE_LEASE_INVOCATION, run_id, lease_generation, completion)
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
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn shutdown_finishes_current_turn_without_leasing_again() {
        let liveness = Liveness::new(Duration::from_secs(90));
        let turns = AtomicUsize::new(0);
        let release_turn = tokio::sync::Notify::new();
        let (entered, entering) = tokio::sync::oneshot::channel();
        let mut entered = Some(entered);
        let (stop, stopping) = tokio::sync::watch::channel(false);
        let serving = serve_queue(
            stopping,
            &liveness,
            async || {
                turns.fetch_add(1, Ordering::Relaxed);
                entered
                    .take()
                    .expect("one turn")
                    .send(())
                    .expect("receiver");
                release_turn.notified().await;
                Ok(true)
            },
            async || Ok(false),
        );
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
    async fn shutdown_before_first_turn_never_leases() {
        let liveness = Liveness::new(Duration::from_secs(90));
        let (_stop, stopping) = tokio::sync::watch::channel(true);
        serve_queue(
            stopping,
            &liveness,
            async || panic!("stopped queue acquired"),
            async || Ok(false),
        )
        .await
        .expect("stopped queue");
        assert_eq!(liveness.silence(), None);
    }

    /// R20 (2): after stop the queue leases the runs pinned to its release
    /// until none remains, then exits. A turn that finds nothing leasable,
    /// because another replica holds the lease, does not end the drain.
    #[tokio::test]
    async fn shutdown_leases_the_pinned_backlog_until_it_is_empty() {
        let liveness = Liveness::new(Duration::from_secs(90));
        let backlog = AtomicUsize::new(2);
        let turns = AtomicUsize::new(0);
        let (_stop, stopping) = tokio::sync::watch::channel(true);
        tokio::time::timeout(
            Duration::from_secs(5),
            serve_queue(
                stopping,
                &liveness,
                async || {
                    // The first turn finds the pinned run leased elsewhere.
                    if turns.fetch_add(1, Ordering::Relaxed) == 0 {
                        return Ok(false);
                    }
                    backlog.fetch_sub(1, Ordering::Relaxed);
                    Ok(true)
                },
                async || Ok(backlog.load(Ordering::Relaxed) > 0),
            ),
        )
        .await
        .expect("the drain ends when the pinned backlog is empty")
        .expect("queue drain");
        assert_eq!(backlog.load(Ordering::Relaxed), 0);
        assert_eq!(turns.load(Ordering::Relaxed), 3);
    }
}

#[cfg(test)]
mod admission_live;
