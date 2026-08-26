//! Host-only composition of one production queue claim transaction.

use std::fmt::{Display, Formatter};

use deadpool_postgres::Object;
use tokio_postgres::Row;
use tokio_postgres::types::FromSql;
use wamn_router::{FailureKind as RouterFailureKind, Outcome, Verdict, WalkStatus};
use wamn_run_state::queue::{
    ProductionClaimClass, advance_claim_attempts_sql, classify_production_claim,
    clear_pre_effect_state_sql, grant_production_claim_sql, renew_production_lease_sql,
    select_claim_effect_attempt_sql, select_exhausted_production_sql, select_production_claim_sql,
    serialize_effect_intent_sql, terminalize_effect_uncertain_claim_sql,
    terminalize_exhausted_production_sql,
};
use wamn_run_state::transitions::{
    CallerReleaseResult, TerminalizeResult, release_caller_sql, terminalize_sql,
};
use wamn_run_state::{
    AuthorityClass, DurabilityClass, EffectUncertainFailure, FailKind, RunStatus,
};

use super::{CandidateBindingWorld, ReleaseIdentity, WamnPostgres};

/// Stable category for a production-claim failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductionClaimErrorKind {
    /// Required host-injected identity was absent.
    Identity,
    /// The admitted run has no complete, valid frozen wiring identity.
    WiringIdentity,
    /// PostgreSQL checkout, transaction, query, or commit failed.
    Storage,
    /// Stored data or a typed database result violated the claim contract.
    Contract,
}

/// Contextual failure from the host-only production claim boundary.
#[derive(Debug)]
pub struct ProductionClaimError {
    kind: ProductionClaimErrorKind,
    operation: &'static str,
    detail: String,
}

impl ProductionClaimError {
    fn new(
        kind: ProductionClaimErrorKind,
        operation: &'static str,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            operation,
            detail: detail.into(),
        }
    }

    /// Return the stable failure category.
    pub fn kind(&self) -> ProductionClaimErrorKind {
        self.kind
    }

    /// Return the operation that failed.
    pub fn operation(&self) -> &'static str {
        self.operation
    }
}

impl Display for ProductionClaimError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "production claim {} failed: {}",
            self.operation, self.detail
        )
    }
}

impl std::error::Error for ProductionClaimError {}

/// Result of one host-only production queue turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductionClaimResult {
    /// No eligible row was visible to this tenant.
    Empty,
    /// A fresh lease committed for router execution.
    Ready {
        run_id: String,
        payload: serde_json::Value,
        lease_generation: i64,
        wiring_id: String,
        wiring_version: i32,
        router_caller_attached: bool,
        durable_caller_attached: bool,
        candidate: Option<ProductionCandidate>,
    },
    /// Claim-time classification removed the row without execution.
    Terminalized {
        run_id: String,
        status: RunStatus,
        fail_kind: FailKind,
    },
}

/// Candidate-only authority frozen on the durable run at admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductionCandidate {
    pub catalog_version: i32,
    pub wiring_hash: String,
    pub binding_world: CandidateBindingWorld,
}

/// Caller result stored before a queue run becomes terminal.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductionCallerOutcome {
    kind: &'static str,
    body: serde_json::Value,
    http_status: u16,
    release_node_id: Option<String>,
}

impl ProductionCallerOutcome {
    /// A router `respond` verdict and its exact wiring node coordinate.
    pub fn responded(
        body: serde_json::Value,
        http_status: u16,
        release_node_id: impl Into<String>,
    ) -> Self {
        Self {
            kind: "responded",
            body,
            http_status,
            release_node_id: Some(release_node_id.into()),
        }
    }

    /// A router failure returned to an attached caller.
    pub fn failed(
        body: serde_json::Value,
        http_status: u16,
        release_node_id: Option<String>,
    ) -> Self {
        Self {
            kind: "failed",
            body,
            http_status,
            release_node_id,
        }
    }
}

/// Storage-shaped terminal fact derived from one router outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct ProductionCompletion {
    status: RunStatus,
    terminal_reason: &'static str,
    result: serde_json::Value,
    fail_kind: Option<FailKind>,
    caller: Option<ProductionCallerOutcome>,
}

impl ProductionCompletion {
    /// A completed router walk, optionally carrying a caller response.
    pub fn completed(result: serde_json::Value, caller: Option<ProductionCallerOutcome>) -> Self {
        Self {
            status: RunStatus::Completed,
            terminal_reason: "router-completed",
            result,
            fail_kind: None,
            caller,
        }
    }

    /// A failed router walk and its persisted failure class.
    pub fn failed(
        result: serde_json::Value,
        fail_kind: FailKind,
        caller: Option<ProductionCallerOutcome>,
    ) -> Self {
        Self {
            status: RunStatus::Failed,
            terminal_reason: "router-failed",
            result,
            fail_kind: Some(fail_kind),
            caller,
        }
    }
}

/// Result of committing a router outcome under the exact queue fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductionCompletionResult {
    Terminalized,
    AlreadyTerminal(RunStatus),
    FenceLost,
    NotFound,
}

/// Result of one generation-fenced lease heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductionLeaseRenewal {
    Renewed,
    FenceLost,
}

/// Boundary work selected from a terminal router outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum ProductionRouterAction {
    /// Commit the run/caller result through the existing fenced transition.
    Complete(ProductionCompletion),
    /// The emit publisher/admission join is owned by `wamn-0h0g.19.8`.
    Emit {
        event: serde_json::Value,
        dedup_id: String,
        entity: String,
        operation: wamn_event_wire::Op,
    },
    /// Cancellation is not a failure verdict; leave the lease to redelivery.
    Cancelled,
}

/// Translate the router taxonomy exactly once at the run-store boundary.
pub fn production_router_action(
    outcome: &Outcome,
    caller_attached: bool,
) -> Result<ProductionRouterAction, ProductionClaimError> {
    production_router_action_with_mode(
        outcome,
        if caller_attached {
            RouterResultMode::DurableCaller
        } else {
            RouterResultMode::Detached
        },
    )
}

/// Translate a management candidate response into the run result without
/// fabricating a synchronous durable caller.
pub fn production_router_result_action(
    outcome: &Outcome,
) -> Result<ProductionRouterAction, ProductionClaimError> {
    production_router_action_with_mode(outcome, RouterResultMode::StoredResult)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouterResultMode {
    Detached,
    DurableCaller,
    StoredResult,
}

fn production_router_action_with_mode(
    outcome: &Outcome,
    mode: RouterResultMode,
) -> Result<ProductionRouterAction, ProductionClaimError> {
    if outcome.status == WalkStatus::Running {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "map router outcome",
            "router-returned-running-outcome",
        ));
    }

    // A verdict may be recorded before the frontier empties. It is the owning
    // boundary truth even when later background work fails or cancels; that
    // later status is observability, not permission to suppress the first
    // caller response or publish.
    if outcome.verdict.is_some()
        && matches!(outcome.status, WalkStatus::Failed | WalkStatus::Cancelled)
    {
        tracing::warn!(
            status = ?outcome.status,
            failure_kind = ?outcome.failure.as_ref().map(|failure| failure.kind),
            failure_node = outcome.failure.as_ref().map(|failure| failure.node.as_str()),
            "router first verdict stood after a later frontier outcome"
        );
    }
    match outcome.verdict.as_ref() {
        Some(Verdict::Respond { payload, node_id }) => match mode {
            RouterResultMode::StoredResult => {
                return Ok(ProductionRouterAction::Complete(
                    ProductionCompletion::completed(payload.clone(), None),
                ));
            }
            RouterResultMode::DurableCaller => {
                let caller =
                    ProductionCallerOutcome::responded(payload.clone(), 200, node_id.clone());
                return Ok(ProductionRouterAction::Complete(
                    ProductionCompletion::completed(payload.clone(), Some(caller)),
                ));
            }
            RouterResultMode::Detached => {
                return Err(ProductionClaimError::new(
                    ProductionClaimErrorKind::Contract,
                    "map router response",
                    "router-response-without-result-owner",
                ));
            }
        },
        Some(Verdict::Emit { event, .. }) if mode == RouterResultMode::StoredResult => {
            return Ok(ProductionRouterAction::Complete(
                ProductionCompletion::completed(event.clone(), None),
            ));
        }
        Some(Verdict::Emit {
            event,
            dedup_id,
            entity,
            operation,
        }) => {
            return Ok(ProductionRouterAction::Emit {
                event: event.clone(),
                dedup_id: dedup_id.clone(),
                entity: entity.clone(),
                operation: *operation,
            });
        }
        Some(Verdict::Discard) if mode != RouterResultMode::DurableCaller => {
            return Ok(ProductionRouterAction::Complete(
                ProductionCompletion::completed(outcome.result.clone(), None),
            ));
        }
        Some(Verdict::Discard) => {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "map router discard",
                "router-discard-with-caller",
            ));
        }
        None => {}
    }

    match outcome.status {
        WalkStatus::Completed => Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "map completed router outcome",
            "router-completed-without-verdict",
        )),
        WalkStatus::Failed => {
            let failure = outcome.failure.as_ref().ok_or_else(|| {
                ProductionClaimError::new(
                    ProductionClaimErrorKind::Contract,
                    "map failed router outcome",
                    "router-failed-without-failure",
                )
            })?;
            let fail_kind = persisted_router_failure(failure.kind);
            let code = failure
                .detail
                .code
                .as_deref()
                .unwrap_or_else(|| router_failure_code(failure.kind));
            let mut error = serde_json::Map::from_iter([
                (
                    "code".to_owned(),
                    serde_json::Value::String(code.to_owned()),
                ),
                (
                    "message".to_owned(),
                    serde_json::Value::String(failure.detail.message.clone()),
                ),
                (
                    "node".to_owned(),
                    serde_json::Value::String(failure.node.clone()),
                ),
            ]);
            if let Some(data) = failure.detail.data.as_ref() {
                error.insert("data".to_owned(), data.clone());
            }
            let body = serde_json::Value::Object(serde_json::Map::from_iter([(
                "error".to_owned(),
                serde_json::Value::Object(error),
            )]));
            let caller = (mode == RouterResultMode::DurableCaller).then(|| {
                ProductionCallerOutcome::failed(body.clone(), 500, Some(failure.node.clone()))
            });
            Ok(ProductionRouterAction::Complete(
                ProductionCompletion::failed(body, fail_kind, caller),
            ))
        }
        WalkStatus::Cancelled => Ok(ProductionRouterAction::Cancelled),
        WalkStatus::Running => unreachable!("running outcomes were refused above"),
    }
}

fn persisted_router_failure(kind: RouterFailureKind) -> FailKind {
    match kind {
        RouterFailureKind::RetryExhausted => FailKind::RetryExhausted,
        RouterFailureKind::InvalidInput => FailKind::InvalidInput,
        RouterFailureKind::HopLimit => FailKind::RunawayBudget,
        RouterFailureKind::Terminal
        | RouterFailureKind::UnreleasedCaller
        | RouterFailureKind::MissingDedupId
        | RouterFailureKind::RespondWithoutCaller
        | RouterFailureKind::SecondVerdict => FailKind::Terminal,
    }
}

fn router_failure_code(kind: RouterFailureKind) -> &'static str {
    match kind {
        RouterFailureKind::Terminal => "terminal",
        RouterFailureKind::RetryExhausted => "retry-exhausted",
        RouterFailureKind::InvalidInput => "invalid-input",
        RouterFailureKind::HopLimit => "hop-limit",
        RouterFailureKind::UnreleasedCaller => "unreleased-caller",
        RouterFailureKind::MissingDedupId => "missing-dedup-id",
        RouterFailureKind::RespondWithoutCaller => "respond-without-caller",
        RouterFailureKind::SecondVerdict => "second-verdict",
    }
}

/// Result of one host-owned crash-budget janitor turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductionReapResult {
    /// No exhausted row was visible to this tenant.
    Empty,
    /// Immutable effect evidence owns this row; the ordinary claimant must
    /// terminalize it as effect-uncertain.
    EffectAttempt { run_id: String },
    /// The selected pre-effect row was marked infrastructure-failure and
    /// dequeued with exact caller compare-and-set semantics.
    Reaped { run_id: String },
}

/// The composed outcome of one claim transaction, decided before COMMIT.
#[derive(Debug)]
enum ClaimTurn {
    /// The transaction may commit and report this result.
    Claimed(ProductionClaimResult),
    /// The lease grant was refused inside its own subtransaction, which the
    /// composer already rolled back. The transaction is still live and MUST
    /// commit so the crash-evidence attempt advance taken before the savepoint
    /// survives — a refusal that rolls its own attempt counter back can never
    /// reach `max_attempts`, so the janitor can never reap the run and it stays
    /// the tenant's FIFO head forever (wamn-0h0g.15.69). The claim itself still
    /// fails with this error.
    GrantRefused(ProductionClaimError),
}

#[derive(Debug)]
struct SelectedClaim {
    run_id: String,
    had_prior_lease: bool,
    status: RunStatus,
    payload: serde_json::Value,
    /// The class the run was admitted under, read off the row this turn already
    /// locked. Everything the crash floor does in this transaction is gated on
    /// it (wamn-0h0g.20.2).
    durability_class: DurabilityClass,
    wiring_id: String,
    wiring_version: i32,
    router_caller_attached: bool,
    durable_caller_attached: bool,
    candidate: Option<ProductionCandidate>,
}

#[derive(Debug)]
struct ExhaustedClaim {
    run_id: String,
    status: RunStatus,
    identity: ExhaustedExecutionIdentity,
    durability_class: DurabilityClass,
}

#[derive(Debug)]
enum ExhaustedExecutionIdentity {
    Flow {
        flow_id: String,
        flow_version: i32,
    },
    Wiring {
        wiring_id: String,
        wiring_version: i32,
    },
}

impl WamnPostgres {
    /// Lock, classify, and lease at most one production run.
    ///
    /// Tenant, project, schema, lease owner, and the carried release identity
    /// come only from host-injected component identity. A `Ready` result is
    /// returned only after COMMIT, so router execution never starts under an
    /// uncommitted lease.
    ///
    /// The lease grant also records this pod's `(release version, manifest
    /// digest)` onto the run, write-once per claim attempt. A component with no
    /// injected release identity records nothing. A pod whose release differs
    /// from a pair the run already carries succeeds because the arm that
    /// reopened the run's claimability already cleared it — the classifier's
    /// pre-effect reclaim, in this same transaction, or the queue park that
    /// released the lease; on any other path the database guard refuses the
    /// claim.
    pub async fn claim_next_production(
        &self,
        component_id: &str,
        catalog_id: &str,
        environment: &str,
        lease_ttl_ms: i64,
    ) -> Result<ProductionClaimResult, ProductionClaimError> {
        if catalog_id.is_empty() || environment.is_empty() || lease_ttl_ms <= 0 {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "validate queue scope",
                "catalog, environment, and positive lease TTL are required",
            ));
        }
        let tenant = self.tenant_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve tenant",
                "component has no host-injected tenant",
            )
        })?;
        let runner = self.runner_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve runner",
                "component has no host-injected runner",
            )
        })?;
        let release = self.release_identity_for(component_id);
        let project = self.project_for(component_id);
        let schema = self.schema_for(component_id);
        let (connection, policy) = self.checkout_platform(&project, AuthorityClass::ExecutorPlatform).await.map_err(|error| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "checkout project connection",
                format!("{error:?}"),
            )
        })?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                &tenant,
                schema.as_deref(),
                Some(&runner),
                None,
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "begin tenant transaction",
                format!("{error:?}"),
            ));
        }

        let result = claim_in_transaction(
            &connection,
            &runner,
            catalog_id,
            environment,
            lease_ttl_ms,
            release.as_ref(),
        )
        .await;
        match result {
            Ok(turn) => {
                if let Err(error) = connection.batch_execute("COMMIT").await {
                    self.destroy(connection);
                    return Err(ProductionClaimError::new(
                        ProductionClaimErrorKind::Storage,
                        "commit production claim",
                        error.to_string(),
                    ));
                }
                match turn {
                    ClaimTurn::Claimed(result) => Ok(result),
                    ClaimTurn::GrantRefused(error) => Err(error),
                }
            }
            Err(error) => {
                if let Err(rollback_error) = connection.batch_execute("ROLLBACK").await {
                    tracing::warn!(
                        error = %rollback_error,
                        operation = error.operation(),
                        "production claim rollback failed; destroying connection"
                    );
                    self.destroy(connection);
                }
                Err(error)
            }
        }
    }

    /// Reap at most one crash-budget-exhausted pre-effect run.
    ///
    /// The candidate and run rows are locked before a fresh effect-evidence
    /// snapshot. Caller JSON and its RFC 8785 hash are computed by the host,
    /// never from PostgreSQL's non-canonical `jsonb::text` rendering.
    pub async fn reap_one_exhausted_production(
        &self,
        component_id: &str,
        catalog_id: &str,
        environment: &str,
        grace_ms: i64,
    ) -> Result<ProductionReapResult, ProductionClaimError> {
        if catalog_id.is_empty() || environment.is_empty() || grace_ms < 0 {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "validate janitor scope",
                "catalog, environment, and non-negative janitor grace are required",
            ));
        }
        let tenant = self.tenant_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve janitor tenant",
                "component has no host-injected tenant",
            )
        })?;
        let runner = self.runner_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve janitor runner",
                "component has no host-injected runner",
            )
        })?;
        let project = self.project_for(component_id);
        let schema = self.schema_for(component_id);
        let (connection, policy) = self.checkout_platform(&project, AuthorityClass::ExecutorPlatform).await.map_err(|error| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "checkout janitor connection",
                format!("{error:?}"),
            )
        })?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                &tenant,
                schema.as_deref(),
                Some(&runner),
                None,
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "begin janitor transaction",
                format!("{error:?}"),
            ));
        }

        let result = reap_in_transaction(&connection, catalog_id, environment, grace_ms).await;
        match result {
            Ok(result) => {
                if let Err(error) = connection.batch_execute("COMMIT").await {
                    self.destroy(connection);
                    return Err(ProductionClaimError::new(
                        ProductionClaimErrorKind::Storage,
                        "commit janitor turn",
                        error.to_string(),
                    ));
                }
                Ok(result)
            }
            Err(error) => {
                if let Err(rollback_error) = connection.batch_execute("ROLLBACK").await {
                    tracing::warn!(
                        error = %rollback_error,
                        operation = error.operation(),
                        "production janitor rollback failed; destroying connection"
                    );
                    self.destroy(connection);
                }
                Err(error)
            }
        }
    }

    /// Extend one claimed run's lease under its exact generation fence.
    pub async fn renew_production_lease(
        &self,
        component_id: &str,
        run_id: &str,
        lease_generation: i64,
        lease_ttl_ms: i64,
    ) -> Result<ProductionLeaseRenewal, ProductionClaimError> {
        if lease_generation <= 0 || lease_ttl_ms <= 0 {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "validate production lease renewal",
                "lease generation and TTL must be positive",
            ));
        }
        let tenant = self.tenant_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve renewal tenant",
                "component has no host-injected tenant",
            )
        })?;
        let runner = self.runner_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve renewal runner",
                "component has no host-injected runner",
            )
        })?;
        let project = self.project_for(component_id);
        let schema = self.schema_for(component_id);
        let (connection, policy) = self.checkout_platform(&project, AuthorityClass::ExecutorPlatform).await.map_err(|error| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "checkout renewal connection",
                format!("{error:?}"),
            )
        })?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                &tenant,
                schema.as_deref(),
                Some(&runner),
                None,
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "begin renewal transaction",
                format!("{error:?}"),
            ));
        }
        let result =
            renew_in_transaction(&connection, run_id, &runner, lease_generation, lease_ttl_ms)
                .await;
        finish_queue_transaction(self, connection, result, "commit production renewal").await
    }

    /// Persist one router terminal outcome and dequeue under the claim fence.
    ///
    /// Caller release and run terminalization share one transaction. An exact
    /// caller replay is accepted; a different winner refuses without changing
    /// the run. `FenceLost` is terminal for this executor turn.
    pub async fn complete_production(
        &self,
        component_id: &str,
        run_id: &str,
        lease_generation: i64,
        completion: &ProductionCompletion,
    ) -> Result<ProductionCompletionResult, ProductionClaimError> {
        if lease_generation <= 0 {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "validate production completion",
                "lease generation must be positive",
            ));
        }
        let tenant = self.tenant_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve completion tenant",
                "component has no host-injected tenant",
            )
        })?;
        let runner = self.runner_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve completion runner",
                "component has no host-injected runner",
            )
        })?;
        let project = self.project_for(component_id);
        let schema = self.schema_for(component_id);
        let (connection, policy) = self.checkout_platform(&project, AuthorityClass::ExecutorPlatform).await.map_err(|error| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "checkout completion connection",
                format!("{error:?}"),
            )
        })?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                &tenant,
                schema.as_deref(),
                Some(&runner),
                None,
                None,
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "begin completion transaction",
                format!("{error:?}"),
            ));
        }
        let result =
            complete_in_transaction(&connection, run_id, &runner, lease_generation, completion)
                .await;
        finish_queue_transaction(self, connection, result, "commit production completion").await
    }
}

async fn finish_queue_transaction<T>(
    postgres: &WamnPostgres,
    connection: Object,
    result: Result<T, ProductionClaimError>,
    commit_operation: &'static str,
) -> Result<T, ProductionClaimError> {
    match result {
        Ok(value) => {
            if let Err(error) = connection.batch_execute("COMMIT").await {
                postgres.destroy(connection);
                return Err(storage(commit_operation, error));
            }
            Ok(value)
        }
        Err(error) => {
            if let Err(rollback_error) = connection.batch_execute("ROLLBACK").await {
                tracing::warn!(
                    error = %rollback_error,
                    operation = error.operation(),
                    "production queue rollback failed; destroying connection"
                );
                postgres.destroy(connection);
            }
            Err(error)
        }
    }
}

async fn renew_in_transaction(
    connection: &Object,
    run_id: &str,
    runner: &str,
    lease_generation: i64,
    lease_ttl_ms: i64,
) -> Result<ProductionLeaseRenewal, ProductionClaimError> {
    let sql = renew_production_lease_sql();
    let statement = connection
        .prepare_cached(&sql)
        .await
        .map_err(|error| storage("prepare production lease renewal", error))?;
    let renewed = connection
        .query_opt(
            &statement,
            &[&run_id, &runner, &lease_generation, &lease_ttl_ms],
        )
        .await
        .map_err(|error| storage("renew production lease", error))?;
    Ok(if renewed.is_some() {
        ProductionLeaseRenewal::Renewed
    } else {
        ProductionLeaseRenewal::FenceLost
    })
}

async fn complete_in_transaction(
    connection: &Object,
    run_id: &str,
    runner: &str,
    lease_generation: i64,
    completion: &ProductionCompletion,
) -> Result<ProductionCompletionResult, ProductionClaimError> {
    if let Some(caller) = completion.caller.as_ref() {
        let body_json = serde_json::to_string(&caller.body).map_err(|error| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "serialize production caller outcome",
                error.to_string(),
            )
        })?;
        let hash = wamn_execution_contract::canonical_json_sha256(&caller.body);
        let http_status = i32::from(caller.http_status);
        let release_node_id = caller.release_node_id.as_deref();
        let sql = release_caller_sql();
        let statement = connection
            .prepare_cached(&sql)
            .await
            .map_err(|error| storage("prepare production caller release", error))?;
        let row = connection
            .query_one(
                &statement,
                &[
                    &run_id,
                    &run_id,
                    &runner,
                    &lease_generation,
                    &caller.kind,
                    &body_json,
                    &http_status,
                    &release_node_id,
                    &hash,
                ],
            )
            .await
            .map_err(|error| storage("release production caller", error))?;
        let release = decode_caller_release(&row)?;
        match release {
            CallerReleaseResult::Released => {}
            CallerReleaseResult::AlreadyReleased(stored)
                if stored.exactly_matches(
                    caller.kind,
                    &caller.body,
                    Some(caller.http_status),
                    release_node_id,
                    &hash,
                ) => {}
            CallerReleaseResult::AlreadyReleased(_) => {
                return Err(ProductionClaimError::new(
                    ProductionClaimErrorKind::Contract,
                    "release production caller",
                    "production-caller-outcome-conflict",
                ));
            }
            CallerReleaseResult::RunTerminal(status) => {
                return Ok(ProductionCompletionResult::AlreadyTerminal(status));
            }
            CallerReleaseResult::FenceLost => {
                return Ok(ProductionCompletionResult::FenceLost);
            }
            CallerReleaseResult::NotFound => {
                return Ok(ProductionCompletionResult::NotFound);
            }
            CallerReleaseResult::CrossRunAuthority => {
                return Err(ProductionClaimError::new(
                    ProductionClaimErrorKind::Contract,
                    "release production caller",
                    "production-cross-run-authority",
                ));
            }
        }
    }

    let result_json = serde_json::to_string(&completion.result).map_err(|error| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "serialize production result",
            error.to_string(),
        )
    })?;
    let fail_kind = completion.fail_kind.map(FailKind::as_sql);
    let sql = terminalize_sql();
    let statement = connection
        .prepare_cached(&sql)
        .await
        .map_err(|error| storage("prepare production terminalization", error))?;
    let row = connection
        .query_one(
            &statement,
            &[
                &run_id,
                &run_id,
                &runner,
                &lease_generation,
                &completion.status.as_sql(),
                &completion.terminal_reason,
                &result_json,
                &fail_kind,
            ],
        )
        .await
        .map_err(|error| storage("terminalize production run", error))?;
    match decode_terminalization(&row)? {
        TerminalizeResult::Terminalized => Ok(ProductionCompletionResult::Terminalized),
        TerminalizeResult::RunTerminal(status) => {
            Ok(ProductionCompletionResult::AlreadyTerminal(status))
        }
        TerminalizeResult::FenceLost => Ok(ProductionCompletionResult::FenceLost),
        TerminalizeResult::NotFound => Ok(ProductionCompletionResult::NotFound),
        TerminalizeResult::CallerUnreleased => Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "terminalize production run",
            "production-caller-unreleased",
        )),
        TerminalizeResult::CrossRunAuthority => Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "terminalize production run",
            "production-cross-run-authority",
        )),
    }
}

fn decode_caller_release(row: &Row) -> Result<CallerReleaseResult, ProductionClaimError> {
    let code: String = row_value(row, 0, "caller release result")?;
    let status: Option<String> = row_value(row, 1, "caller release run status")?;
    let kind: Option<String> = row_value(row, 2, "caller outcome kind")?;
    let body_text: Option<String> = row_value(row, 3, "caller outcome body")?;
    let body = body_text
        .map(|body| serde_json::from_str(&body))
        .transpose()
        .map_err(|error| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "decode production caller outcome",
                error.to_string(),
            )
        })?;
    let http_status: Option<i32> = row_value(row, 4, "caller outcome HTTP status")?;
    let http_status = http_status
        .map(u16::try_from)
        .transpose()
        .map_err(|error| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "decode production caller outcome",
                error.to_string(),
            )
        })?;
    let release_node_id = row_value(row, 5, "caller release node")?;
    let hash = row_value(row, 6, "caller outcome hash")?;
    CallerReleaseResult::from_parts(
        &code,
        status.as_deref().unwrap_or_default(),
        kind,
        body,
        http_status,
        release_node_id,
        hash,
    )
    .ok_or_else(|| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "decode production caller release",
            format!("unknown or incomplete result {code:?}"),
        )
    })
}

fn decode_terminalization(row: &Row) -> Result<TerminalizeResult, ProductionClaimError> {
    let code: String = row_value(row, 0, "terminalization result")?;
    let status: Option<String> = row_value(row, 1, "terminalization run status")?;
    TerminalizeResult::from_parts(&code, status.as_deref().unwrap_or_default()).ok_or_else(|| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "decode production terminalization",
            format!("unknown or incomplete result {code:?}"),
        )
    })
}

async fn claim_in_transaction(
    connection: &Object,
    runner: &str,
    catalog_id: &str,
    environment: &str,
    lease_ttl_ms: i64,
    release: Option<&ReleaseIdentity>,
) -> Result<ClaimTurn, ProductionClaimError> {
    let select_sql = select_production_claim_sql();
    let select = connection
        .prepare_cached(&select_sql)
        .await
        .map_err(|error| storage("prepare production candidate", error))?;
    let Some(row) = connection
        .query_opt(&select, &[&catalog_id, &environment])
        .await
        .map_err(|error| storage("select production candidate", error))?
    else {
        return Ok(ClaimTurn::Claimed(ProductionClaimResult::Empty));
    };
    let selected = decode_selected_claim(&row)?;
    if !matches!(selected.status, RunStatus::Dispatched | RunStatus::Running) {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "validate selected run",
            format!(
                "queue row {} references non-runnable status {}",
                selected.run_id,
                selected.status.as_sql()
            ),
        ));
    }

    // THE CLASS GATE (wamn-0h0g.20.2). The default class takes plain
    // lock-then-lease: no advisory fence, no effect snapshot, no classification
    // — the two statements below exist ONLY to read immutable effect evidence,
    // and evidence a `standard` run's claim path may not act on is evidence it
    // must not pay to read on the queue's hottest turn. The premium class takes
    // today's lock-then-classify-then-lease, unchanged, byte for byte.
    let has_effect_attempt = if selected.durability_class.admits_effect_evidence() {
        serialize_effect_intent(connection, &selected.run_id, "production claim").await?;

        let effect_sql = select_claim_effect_attempt_sql();
        let effect_statement = connection
            .prepare_cached(&effect_sql)
            .await
            .map_err(|error| storage("prepare effect-attempt classification", error))?;
        let effect_row = connection
            .query_one(&effect_statement, &[&selected.run_id])
            .await
            .map_err(|error| storage("classify effect-attempt evidence", error))?;
        row_value(&effect_row, 0, "effect-attempt evidence")?
    } else {
        false
    };

    match classify_production_claim(
        selected.durability_class,
        selected.had_prior_lease,
        has_effect_attempt,
    ) {
        ProductionClaimClass::ExpiredWithAttempt => {
            return terminalize_effect_uncertain(connection, &selected)
                .await
                .map(ClaimTurn::Claimed);
        }
        ProductionClaimClass::ExpiredPreEffect => {
            let clear_sql = clear_pre_effect_state_sql();
            let clear = connection
                .prepare_cached(&clear_sql)
                .await
                .map_err(|error| storage("prepare pre-effect state clear", error))?;
            connection
                .query_one(&clear, &[&selected.run_id])
                .await
                .map_err(|error| storage("clear pre-effect state", error))?;
        }
        // Nothing to reset here. A never-leased row carries no record, and a
        // queue-parked one had its record cleared by the park that released
        // the lease: the arm that REOPENS claimability owns the clear
        // (wamn-0h0g.15.82), so the grant below always writes over NULL.
        ProductionClaimClass::Ordinary => {}
    }

    // Crash evidence advances on every path that reaches the grant, in its own
    // statement OUTSIDE the grant's subtransaction. Terminalization returns
    // above and still does not count.
    let advance_sql = advance_claim_attempts_sql();
    let advance = connection
        .prepare_cached(&advance_sql)
        .await
        .map_err(|error| storage("prepare crash-evidence advance", error))?;
    connection
        .query_one(&advance, &[&selected.run_id])
        .await
        .map_err(|error| storage("advance crash evidence", error))?;

    let grant_sql = grant_production_claim_sql();
    let grant = connection
        .prepare_cached(&grant_sql)
        .await
        .map_err(|error| storage("prepare production lease grant", error))?;
    // The pod's own release identity, or NULL for both when it carries none.
    // A run that already recorded a DIFFERENT pair refuses here, in the
    // database (wamn-0h0g.15.55): the classifier's pre-effect reclaim is the
    // only path that may clear the pair, and it does so above.
    let release = release.filter(|_| selected.candidate.is_none());
    let release_version: Option<i32> = release.map(|identity| identity.release_version);
    let manifest_digest: Option<&str> = release.map(|identity| identity.manifest_digest.as_str());
    // The grant is the one abortable write left in this transaction, so it runs
    // in its own subtransaction: a database refusal rolls back to the savepoint
    // instead of the whole transaction, leaving the advance above committable.
    connection
        .batch_execute("SAVEPOINT wamn_production_grant")
        .await
        .map_err(|error| storage("open production lease savepoint", error))?;
    let granted = connection
        .query_opt(
            &grant,
            &[
                &selected.run_id,
                &runner,
                &lease_ttl_ms,
                &release_version,
                &manifest_digest,
            ],
        )
        .await;
    let granted = match granted {
        Ok(granted) => granted,
        Err(error) => {
            connection
                .batch_execute("ROLLBACK TO SAVEPOINT wamn_production_grant")
                .await
                .map_err(|rollback| storage("roll back refused production lease", rollback))?;
            return Ok(ClaimTurn::GrantRefused(storage(
                "grant production lease",
                error,
            )));
        }
    };
    let row = granted.ok_or_else(|| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "grant production lease",
            "selected queue row disappeared while locked",
        )
    })?;
    let lease_generation = row_value(&row, 0, "lease generation")?;
    Ok(ClaimTurn::Claimed(ProductionClaimResult::Ready {
        run_id: selected.run_id,
        payload: selected.payload,
        lease_generation,
        wiring_id: selected.wiring_id,
        wiring_version: selected.wiring_version,
        router_caller_attached: selected.router_caller_attached,
        durable_caller_attached: selected.durable_caller_attached,
        candidate: selected.candidate,
    }))
}

async fn reap_in_transaction(
    connection: &Object,
    catalog_id: &str,
    environment: &str,
    grace_ms: i64,
) -> Result<ProductionReapResult, ProductionClaimError> {
    let select_sql = select_exhausted_production_sql();
    let select = connection
        .prepare_cached(&select_sql)
        .await
        .map_err(|error| storage("prepare exhausted candidate", error))?;
    let Some(row) = connection
        .query_opt(&select, &[&grace_ms, &catalog_id, &environment])
        .await
        .map_err(|error| storage("select exhausted candidate", error))?
    else {
        return Ok(ProductionReapResult::Empty);
    };
    let status_text: String = row_value(&row, 2, "exhausted run status")?;
    let status = RunStatus::from_sql(&status_text).ok_or_else(|| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "decode exhausted candidate",
            format!("unknown run status {status_text:?}"),
        )
    })?;
    let class_text: String = row_value(&row, 5, "exhausted run durability class")?;
    let flow_id: Option<String> = row_value(&row, 3, "exhausted root flow id")?;
    let flow_version: Option<i32> = row_value(&row, 4, "exhausted root flow version")?;
    let wiring_id: Option<String> = row_value(&row, 6, "exhausted wiring id")?;
    let wiring_version: Option<i32> = row_value(&row, 7, "exhausted wiring version")?;
    let identity = match (flow_id, flow_version, wiring_id, wiring_version) {
        (Some(flow_id), Some(flow_version), _, _) if !flow_id.is_empty() && flow_version > 0 => {
            ExhaustedExecutionIdentity::Flow {
                flow_id,
                flow_version,
            }
        }
        (None, None, Some(wiring_id), Some(wiring_version))
            if !wiring_id.is_empty() && wiring_version > 0 =>
        {
            ExhaustedExecutionIdentity::Wiring {
                wiring_id,
                wiring_version,
            }
        }
        _ => {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "decode exhausted candidate",
                "run-execution-grain-corrupt",
            ));
        }
    };
    let selected = ExhaustedClaim {
        run_id: row_value(&row, 1, "exhausted run id")?,
        status,
        identity,
        durability_class: DurabilityClass::from_sql_or_default(&class_text),
    };
    if !matches!(selected.status, RunStatus::Dispatched | RunStatus::Running) {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "validate exhausted candidate",
            format!("non-runnable status {}", selected.status.as_sql()),
        ));
    }

    // The same class gate the claim turn applies (wamn-0h0g.20.2). A `standard`
    // run has no effect-uncertain hand-off to make, so the janitor reaps it to
    // `infrastructure-failure` directly and `ProductionReapResult::EffectAttempt`
    // is unreachable — the variant survives for the premium tier.
    if selected.durability_class.admits_effect_evidence() {
        serialize_effect_intent(connection, &selected.run_id, "exhausted-run reaper").await?;

        let effect_sql = select_claim_effect_attempt_sql();
        let effect = connection
            .prepare_cached(&effect_sql)
            .await
            .map_err(|error| storage("prepare exhausted effect classification", error))?;
        let row = connection
            .query_one(&effect, &[&selected.run_id])
            .await
            .map_err(|error| storage("classify exhausted effect evidence", error))?;
        if row_value(&row, 0, "exhausted effect-attempt evidence")? {
            return Ok(ProductionReapResult::EffectAttempt {
                run_id: selected.run_id,
            });
        }
    }

    let (body, body_hash) = generic_failure_outcome(
        "infrastructure-failure",
        &selected.identity,
        &selected.run_id,
    )?;
    let terminalize_sql = terminalize_exhausted_production_sql();
    let terminalize = connection
        .prepare_cached(&terminalize_sql)
        .await
        .map_err(|error| storage("prepare exhausted terminalization", error))?;
    let row = connection
        .query_opt(&terminalize, &[&selected.run_id, &body, &body_hash])
        .await
        .map_err(|error| storage("terminalize exhausted run", error))?
        .ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "terminalize exhausted run",
                "selected run or queue row disappeared while locked",
            )
        })?;
    let status: String = row_value(&row, 0, "exhausted terminal status")?;
    if status != RunStatus::InfrastructureFailure.as_sql() {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "terminalize exhausted run",
            format!("unexpected status {status:?}"),
        ));
    }
    Ok(ProductionReapResult::Reaped {
        run_id: selected.run_id,
    })
}

async fn serialize_effect_intent(
    connection: &Object,
    run_id: &str,
    owner: &'static str,
) -> Result<(), ProductionClaimError> {
    let sql = serialize_effect_intent_sql();
    let statement = connection
        .prepare_cached(&sql)
        .await
        .map_err(|error| storage("prepare effect-intent fence", error))?;
    connection
        .query_one(&statement, &[&run_id])
        .await
        .map_err(|error| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "acquire effect-intent fence",
                format!("{owner}: {error}"),
            )
        })?;
    Ok(())
}

async fn terminalize_effect_uncertain(
    connection: &Object,
    selected: &SelectedClaim,
) -> Result<ProductionClaimResult, ProductionClaimError> {
    let failure = EffectUncertainFailure::new(selected.run_id.clone()).map_err(|error| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "build effect-uncertain outcome",
            error.to_string(),
        )
    })?;
    let body = serde_json::to_string(&failure.as_json()).map_err(|error| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "serialize effect-uncertain outcome",
            error.to_string(),
        )
    })?;
    let hash = failure.canonical_json_hash();
    let sql = terminalize_effect_uncertain_claim_sql();
    let statement = connection
        .prepare_cached(&sql)
        .await
        .map_err(|error| storage("prepare effect-uncertain terminalization", error))?;
    let row = connection
        .query_opt(&statement, &[&selected.run_id, &body, &hash])
        .await
        .map_err(|error| storage("terminalize effect uncertainty", error))?
        .ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "terminalize effect uncertainty",
                "selected run or queue row disappeared while locked",
            )
        })?;
    let status: String = row_value(&row, 0, "effect-uncertain status")?;
    if status != RunStatus::EffectUncertain.as_sql() {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "terminalize effect uncertainty",
            format!("unexpected status {status:?}"),
        ));
    }
    Ok(ProductionClaimResult::Terminalized {
        run_id: selected.run_id.clone(),
        status: RunStatus::EffectUncertain,
        fail_kind: FailKind::EffectUncertain,
    })
}

fn generic_failure_outcome(
    code: &str,
    identity: &ExhaustedExecutionIdentity,
    run_id: &str,
) -> Result<(String, String), ProductionClaimError> {
    let coordinate = match identity {
        ExhaustedExecutionIdentity::Flow {
            flow_id,
            flow_version,
        } => serde_json::json!({
            "code": code,
            "flow-id": flow_id,
            "flow-version": flow_version,
            "run-id": run_id,
        }),
        ExhaustedExecutionIdentity::Wiring {
            wiring_id,
            wiring_version,
        } => serde_json::json!({
            "code": code,
            "wiring-id": wiring_id,
            "wiring-version": wiring_version,
            "run-id": run_id,
        }),
    };
    let body = serde_json::json!({ "error": coordinate });
    let body_hash = wamn_execution_contract::canonical_json_sha256(&body);
    let body_json = serde_json::to_string(&body).map_err(|error| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "serialize generic failure outcome",
            error.to_string(),
        )
    })?;
    Ok((body_json, body_hash))
}

fn decode_selected_claim(row: &Row) -> Result<SelectedClaim, ProductionClaimError> {
    let status: String = row_value(row, 2, "run status")?;
    let status = RunStatus::from_sql(&status).ok_or_else(|| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "decode production candidate",
            format!("unknown run status {status:?}"),
        )
    })?;
    // An unknown literal decodes to the CHEAP tier, never to `durable`
    // (wamn-0h0g.20.1): a claim must not enroll a run in premium machinery on
    // the strength of data it could not parse, and it must not fail the queue
    // either.
    let class_text: String = row_value(row, 4, "durability class")?;
    let wiring_id: Option<String> = row_value(row, 5, "wiring id")?;
    let wiring_version: Option<i32> = row_value(row, 6, "wiring version")?;
    let router_caller_attached: bool = row_value(row, 7, "router caller attachment")?;
    let durable_caller_attached: bool = row_value(row, 8, "durable caller attachment")?;
    let flow_id: Option<String> = row_value(row, 9, "legacy flow id")?;
    let flow_version: Option<i32> = row_value(row, 10, "legacy flow version")?;
    let catalog_version: i32 = row_value(row, 11, "catalog version")?;
    let wiring_hash: Option<String> = row_value(row, 12, "candidate wiring hash")?;
    let binding_world: Option<String> = row_value(row, 13, "candidate binding world")?;
    let payload_text: String = row_value(row, 3, "authoritative input")?;
    let payload = serde_json::from_str(&payload_text).map_err(|error| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "decode production candidate",
            format!("authoritative input: {error}"),
        )
    })?;
    let (wiring_id, wiring_version) = decode_wiring_identity(wiring_id, wiring_version)?;
    let candidate = match (flow_id, flow_version, wiring_hash, binding_world) {
        (Some(flow_id), Some(flow_version), None, None)
            if !flow_id.is_empty() && flow_version > 0 =>
        {
            None
        }
        (None, None, Some(wiring_hash), Some(binding_world))
            if catalog_version > 0 && !wiring_hash.is_empty() =>
        {
            let binding_world = serde_json::from_str(&binding_world)
                .map_err(|error| {
                    ProductionClaimError::new(
                        ProductionClaimErrorKind::Contract,
                        "decode production candidate",
                        format!("candidate-binding-world-json-invalid: {error}"),
                    )
                })
                .and_then(|value| {
                    CandidateBindingWorld::from_json(value).map_err(|error| {
                        ProductionClaimError::new(
                            ProductionClaimErrorKind::Contract,
                            "decode production candidate",
                            error.to_string(),
                        )
                    })
                })?;
            Some(ProductionCandidate {
                catalog_version,
                wiring_hash,
                binding_world,
            })
        }
        _ => {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "decode production candidate",
                "run-execution-grain-corrupt",
            ));
        }
    };
    if candidate.is_some() && (!router_caller_attached || durable_caller_attached) {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "decode production candidate",
            "candidate-caller-grain-corrupt",
        ));
    }
    Ok(SelectedClaim {
        run_id: row_value(row, 0, "run id")?,
        had_prior_lease: row_value(row, 1, "prior lease evidence")?,
        status,
        payload,
        durability_class: DurabilityClass::from_sql_or_default(&class_text),
        wiring_id,
        wiring_version,
        router_caller_attached,
        durable_caller_attached,
        candidate,
    })
}

fn decode_wiring_identity(
    wiring_id: Option<String>,
    wiring_version: Option<i32>,
) -> Result<(String, i32), ProductionClaimError> {
    let (wiring_id, wiring_version) = match (wiring_id, wiring_version) {
        (Some(wiring_id), Some(wiring_version)) if !wiring_id.is_empty() && wiring_version > 0 => {
            (wiring_id, wiring_version)
        }
        (None, None) => {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::WiringIdentity,
                "decode production candidate",
                "run-wiring-identity-missing",
            ));
        }
        _ => {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::WiringIdentity,
                "decode production candidate",
                "run-wiring-identity-corrupt",
            ));
        }
    };
    Ok((wiring_id, wiring_version))
}

fn row_value<T>(row: &Row, index: usize, field: &'static str) -> Result<T, ProductionClaimError>
where
    for<'value> T: FromSql<'value>,
{
    row.try_get(index).map_err(|error| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "decode production claim row",
            format!("{field}: {error}"),
        )
    })
}

/// Build a storage failure that still carries what PostgreSQL actually said.
///
/// `tokio_postgres::Error`'s `Display` renders a database failure as the literal
/// string `"db error"` and appends NOTHING (0.7.18, `error/mod.rs:394`) — the
/// `DbError` is reachable only through `source()`. A detail built from
/// `to_string()` alone therefore discards the message, the SQLSTATE, and the
/// constraint or trigger name, so every refused claim reads identically in logs
/// and in a caller's assertion. The database's own text is the whole diagnostic
/// value of this error, so it is spliced back in here.
fn storage(operation: &'static str, error: tokio_postgres::Error) -> ProductionClaimError {
    let detail = match error.as_db_error() {
        Some(db_error) => format!("{error}: {db_error}"),
        None => error.to_string(),
    };
    ProductionClaimError::new(ProductionClaimErrorKind::Storage, operation, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_and_corrupt_wiring_identity_are_dedicated_stable_refusals() {
        let missing = decode_wiring_identity(None, None).unwrap_err();
        assert_eq!(missing.kind(), ProductionClaimErrorKind::WiringIdentity);
        assert!(missing.to_string().contains("run-wiring-identity-missing"));

        for corrupt in [
            decode_wiring_identity(Some("orders".into()), None),
            decode_wiring_identity(None, Some(1)),
            decode_wiring_identity(Some(String::new()), Some(1)),
            decode_wiring_identity(Some("orders".into()), Some(0)),
        ] {
            let error = corrupt.unwrap_err();
            assert_eq!(error.kind(), ProductionClaimErrorKind::WiringIdentity);
            assert!(error.to_string().contains("run-wiring-identity-corrupt"));
        }
    }


    #[test]
    fn generic_refusal_body_is_exact_and_message_free() {
        let identity = ExhaustedExecutionIdentity::Flow {
            flow_id: "root".to_owned(),
            flow_version: 7,
        };
        let (json, hash) = generic_failure_outcome("foreign-revision", &identity, "run-1").unwrap();
        let body = serde_json::from_str::<serde_json::Value>(&json).unwrap();
        assert_eq!(
            body,
            serde_json::from_str::<serde_json::Value>(
                r#"{"error":{"code":"foreign-revision","flow-id":"root","flow-version":7,"run-id":"run-1"}}"#,
            )
            .unwrap()
        );
        assert!(body["error"].get("message").is_none());
        assert_eq!(hash, wamn_execution_contract::canonical_json_sha256(&body));
    }

    #[test]
    fn janitor_failure_body_uses_host_jcs_not_database_json_text() {
        let identity = ExhaustedExecutionIdentity::Flow {
            flow_id: "root-flow".to_owned(),
            flow_version: 19,
        };
        let (json, hash) =
            generic_failure_outcome("infrastructure-failure", &identity, "run-exhausted").unwrap();
        assert_eq!(
            json,
            r#"{"error":{"code":"infrastructure-failure","flow-id":"root-flow","flow-version":19,"run-id":"run-exhausted"}}"#
        );
        let body = serde_json::from_str(&json).unwrap();
        assert_eq!(hash, wamn_execution_contract::canonical_json_sha256(&body));
    }

    #[test]
    fn candidate_janitor_failure_body_names_the_frozen_wiring() {
        let identity = ExhaustedExecutionIdentity::Wiring {
            wiring_id: "candidate-orders".to_owned(),
            wiring_version: 4,
        };
        let (json, _) =
            generic_failure_outcome("infrastructure-failure", &identity, "case-report-7-0")
                .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&json).unwrap(),
            serde_json::json!({
                "error": {
                    "code": "infrastructure-failure",
                    "wiring-id": "candidate-orders",
                    "wiring-version": 4,
                    "run-id": "case-report-7-0"
                }
            })
        );
    }

    #[test]
    fn candidate_respond_is_stored_without_fabricating_a_durable_caller() {
        let payload = serde_json::json!({"accepted": true});
        let outcome = Outcome {
            status: WalkStatus::Completed,
            result: serde_json::Value::Null,
            failure: None,
            hops: 1,
            verdict: Some(Verdict::Respond {
                payload: payload.clone(),
                node_id: "candidate-respond".into(),
            }),
        };
        let ProductionRouterAction::Complete(completion) =
            production_router_result_action(&outcome).expect("candidate response maps")
        else {
            panic!("candidate response must complete the run");
        };
        assert_eq!(completion.status, RunStatus::Completed);
        assert_eq!(completion.result, payload);
        assert!(completion.caller.is_none());
    }

    #[test]
    fn durable_respond_carries_the_host_terminal_node_through_completion() {
        let payload = serde_json::json!({
            "accepted": true,
            "release-node-id": "guest-forged"
        });
        for status in [
            WalkStatus::Completed,
            WalkStatus::Failed,
            WalkStatus::Cancelled,
        ] {
            let outcome = Outcome {
                status,
                result: serde_json::Value::Null,
                failure: (status == WalkStatus::Failed).then(|| wamn_router::Failure {
                    node: "later".into(),
                    kind: RouterFailureKind::Terminal,
                    detail: wamn_router::ErrorDetail::msg("later work failed"),
                }),
                hops: 2,
                verdict: Some(Verdict::Respond {
                    payload: payload.clone(),
                    node_id: "wiring-terminal".into(),
                }),
            };
            let ProductionRouterAction::Complete(completion) =
                production_router_action(&outcome, true).expect("durable response maps")
            else {
                panic!("durable response must complete the queue run");
            };

            assert_eq!(completion.status, RunStatus::Completed);
            assert_eq!(completion.result, payload);
            let caller = completion.caller.expect("durable caller is released");
            assert_eq!(caller.kind, "responded");
            assert_eq!(caller.body, payload);
            assert_eq!(caller.http_status, 200);
            assert_eq!(caller.release_node_id.as_deref(), Some("wiring-terminal"));
        }
    }

    #[test]
    fn detached_respond_still_has_no_result_owner() {
        let outcome = Outcome {
            status: WalkStatus::Completed,
            result: serde_json::Value::Null,
            failure: None,
            hops: 1,
            verdict: Some(Verdict::Respond {
                payload: serde_json::json!({"accepted": true}),
                node_id: "respond".into(),
            }),
        };

        let error = production_router_action(&outcome, false)
            .expect_err("detached delivery cannot own a response");
        assert_eq!(error.kind(), ProductionClaimErrorKind::Contract);
        assert!(
            error
                .to_string()
                .contains("router-response-without-result-owner")
        );
    }




    #[test]
    fn lease_grant_uses_a_fresh_post_fence_clock() {
        let lease_sql = grant_production_claim_sql();
        assert!(lease_sql.contains("lease_expires_at = statement_timestamp()"));
        assert!(!lease_sql.contains("lease_expires_at = now()"));
    }

    #[test]
    fn lease_grant_mints_the_claim_time_release_record_on_the_existing_write() {
        let lease_sql = grant_production_claim_sql();
        for required in [
            "SET status = 'running'",
            "release_version = $4",
            "manifest_digest = $5",
            "r.status IN ('dispatched', 'running')",
        ] {
            assert!(lease_sql.contains(required), "lease grant omits {required}");
        }
        assert_eq!(
            lease_sql.matches("UPDATE runs").count(),
            1,
            "the record is minted on the existing claim write, not a second one"
        );

        // The pair travels from the claiming pod, so the candidate select never
        // reads it back; a decoder that grew a field would need this to change.
        let select_sql = select_production_claim_sql();
        assert!(!select_sql.contains("release_version"));
        assert!(!select_sql.contains("manifest_digest"));
    }

    #[test]
    fn candidate_projection_is_exactly_what_the_decoder_indexes() {
        let sql = select_production_claim_sql();
        let start = sql
            .find("SELECT candidate.run_id")
            .expect("the outer projection opens on the run id");
        let projection = &sql[start..];
        let mut cursor = 0;
        for (index, column) in [
            "candidate.run_id",
            "candidate.had_prior_lease",
            "r.status",
            "AS input_json",
            "r.durability_class",
            "r.wiring_id",
            "r.wiring_version",
            "AS router_caller_attached",
            "AS durable_caller_attached",
            "r.flow_id",
            "r.flow_version",
            "r.catalog_version",
            "r.wiring_hash",
            "r.binding_world_json::text",
        ]
        .into_iter()
        .enumerate()
        {
            let offset = projection[cursor..].find(column).unwrap_or_else(|| {
                panic!("projected column {index} ({column}) is absent or out of order")
            });
            cursor += offset + column.len();
        }
        assert!(!projection.contains("execution_bundle_hash"));
    }

    #[test]
    fn lease_renewal_is_generation_fenced_and_uses_a_fresh_clock() {
        let sql = renew_production_lease_sql();
        assert!(sql.contains("q.lease_owner = $2"));
        assert!(sql.contains("q.lease_generation = $3"));
        assert!(sql.contains("statement_timestamp()"));
        assert!(sql.contains("q.lease_expires_at > statement_timestamp()"));
        assert!(!sql.contains("execution_bundle_hash"));
    }

    #[test]
    fn router_failure_maps_once_to_run_and_caller_truth() {
        let outcome = Outcome {
            status: WalkStatus::Failed,
            result: serde_json::Value::Null,
            failure: Some(wamn_router::Failure {
                node: "validate".into(),
                kind: RouterFailureKind::InvalidInput,
                detail: wamn_router::ErrorDetail::coded("bad-order", "order is malformed"),
            }),
            hops: 1,
            verdict: None,
        };
        let ProductionRouterAction::Complete(completion) =
            production_router_action(&outcome, true).expect("failed walk maps")
        else {
            panic!("failed walk must complete the run");
        };
        assert_eq!(completion.status, RunStatus::Failed);
        assert_eq!(completion.fail_kind, Some(FailKind::InvalidInput));
        assert_eq!(completion.result["error"]["code"], "bad-order");
        assert_eq!(completion.result["error"]["node"], "validate");
        let caller = completion.caller.expect("attached caller gets failure");
        assert_eq!(caller.kind, "failed");
        assert_eq!(caller.release_node_id.as_deref(), Some("validate"));
    }

    #[test]
    fn callerless_discard_completes_without_caller_projection() {
        let outcome = Outcome {
            status: WalkStatus::Completed,
            result: serde_json::json!({"ok": true}),
            failure: None,
            hops: 1,
            verdict: Some(Verdict::Discard),
        };
        let ProductionRouterAction::Complete(completion) =
            production_router_action(&outcome, false).expect("discard maps")
        else {
            panic!("discard must complete the run");
        };
        assert_eq!(completion.status, RunStatus::Completed);
        assert_eq!(completion.fail_kind, None);
        assert_eq!(completion.caller, None);
    }

    #[test]
    fn first_emit_verdict_wins_over_later_frontier_failure_or_cancellation() {
        for status in [WalkStatus::Failed, WalkStatus::Cancelled] {
            let outcome = Outcome {
                status,
                result: serde_json::Value::Null,
                failure: (status == WalkStatus::Failed).then(|| wamn_router::Failure {
                    node: "later".into(),
                    kind: RouterFailureKind::SecondVerdict,
                    detail: wamn_router::ErrorDetail::coded(
                        "second-verdict",
                        "later frontier reached another terminal",
                    ),
                }),
                hops: 2,
                verdict: Some(Verdict::Emit {
                    event: serde_json::json!({"order": 42}),
                    dedup_id: "wiring-1:7:first:d1".into(),
                    entity: "orders".into(),
                    operation: wamn_event_wire::Op::Insert,
                }),
            };

            assert_eq!(
                production_router_action(&outcome, false).expect("first verdict maps"),
                ProductionRouterAction::Emit {
                    event: serde_json::json!({"order": 42}),
                    dedup_id: "wiring-1:7:first:d1".into(),
                    entity: "orders".into(),
                    operation: wamn_event_wire::Op::Insert,
                },
                "later {status:?} must not suppress the first emit verdict"
            );
        }
    }

    #[test]
    fn candidate_emit_is_a_stored_observable_not_a_boundary_effect() {
        let event = serde_json::json!({"order": 42, "dedup-id": "d1"});
        let outcome = Outcome {
            status: WalkStatus::Completed,
            result: serde_json::Value::Null,
            failure: None,
            hops: 1,
            verdict: Some(Verdict::Emit {
                event: event.clone(),
                dedup_id: "d1".into(),
                entity: "orders".into(),
                operation: wamn_event_wire::Op::Insert,
            }),
        };
        let ProductionRouterAction::Complete(completion) =
            production_router_result_action(&outcome).expect("candidate observable maps")
        else {
            panic!("candidate emit must not request production publication");
        };
        assert_eq!(completion.result, event);
        assert!(completion.caller.is_none());
    }

    #[test]
    fn running_outcome_is_refused_even_if_it_carries_a_verdict() {
        let outcome = Outcome {
            status: WalkStatus::Running,
            result: serde_json::Value::Null,
            failure: None,
            hops: 1,
            verdict: Some(Verdict::Discard),
        };

        let error = production_router_action(&outcome, false)
            .expect_err("an in-progress router result is not a queue terminal");
        assert_eq!(error.kind(), ProductionClaimErrorKind::Contract);
        assert!(
            error
                .to_string()
                .contains("router-returned-running-outcome")
        );
    }

    #[test]
    fn the_default_class_never_reaches_the_crash_floor_arms() {
        // (b), (c) and (d) of wamn-0h0g.20.2 are made UNREACHABLE by the gate at
        // (a) rather than deleted: `classify_production_claim` is the only
        // producer of `ExpiredWithAttempt`, which is the only path to
        // `terminalize_effect_uncertain` and so to `ProductionClaimResult::
        // Terminalized` and its drain-loop arm.
        for had_prior_lease in [false, true] {
            for has_effect_attempt in [false, true] {
                let class = classify_production_claim(
                    DurabilityClass::Standard,
                    had_prior_lease,
                    has_effect_attempt,
                );
                assert_ne!(
                    class,
                    ProductionClaimClass::ExpiredWithAttempt,
                    "the default class reached the shelved floor \
                     (had_prior_lease={had_prior_lease}, attempt={has_effect_attempt})"
                );
            }
        }
        assert_eq!(
            classify_production_claim(DurabilityClass::Durable, true, true),
            ProductionClaimClass::ExpiredWithAttempt,
            "the premium class no longer reaches the floor it pays for"
        );
    }

}
