//! Host-only composition of one production queue claim transaction.

use std::time::SystemTime;

use deadpool_postgres::Object;
use tokio_postgres::Row;
use tokio_postgres::types::FromSql;
use wamn_run_state::authority_class::CURRENT_USER_ROLE_MEMBERSHIP_SQL;
use wamn_run_state::queue::{
    ProductionClaimClass, advance_claim_attempts_sql, classify_production_claim,
    clear_pre_effect_state_sql, grant_production_claim_sql, renew_production_lease_sql,
    select_claim_effect_attempt_sql, select_exhausted_production_sql, select_production_claim_sql,
    serialize_effect_intent_sql, terminalize_effect_uncertain_claim_sql,
    terminalize_exhausted_production_sql,
};
use wamn_run_state::run_store::RunStore;
pub use wamn_run_state::run_store::{
    ProductionCallerOutcome, ProductionClaimError, ProductionClaimErrorKind, ProductionCompletion,
    ProductionCompletionResult, ProductionLeaseRenewal, ProductionReapResult,
};
use wamn_run_state::transitions::{
    CallerReleaseResult, TerminalizeResult, release_caller_sql, terminalize_sql,
};
use wamn_run_state::{
    AuthorityClass, DurabilityClass, EffectUncertainFailure, FailKind, RunStatus,
};

use super::{CandidateBindingWorld, ReleaseIdentity, WamnPostgres};

/// Result of one host-only production queue turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductionClaimResult {
    /// No eligible row was visible to this tenant.
    Empty,
    /// A fresh lease committed for router execution.
    Ready {
        run_id: String,
        package_id: String,
        payload: serde_json::Value,
        lease_generation: i64,
        wiring_id: String,
        wiring_version: i32,
        router_caller_attached: bool,
        durable_caller_attached: bool,
        candidate: Option<ProductionCandidate>,
        service_principal_id: Option<String>,
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
    pub effective_release_id: i32,
    pub wiring_hash: String,
    pub binding_world: CandidateBindingWorld,
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
    package_id: String,
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
    service_principal_id: Option<String>,
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

#[async_trait::async_trait]
impl RunStore for WamnPostgres {
    type ClaimResult = ProductionClaimResult;

    /// Lock, classify, and lease at most one production run.
    ///
    /// Tenant, project, schema, lease owner, and the carried release identity
    /// come only from host-injected component identity. A `Ready` result is
    /// returned only after COMMIT, so router execution never starts under an
    /// uncommitted lease.
    ///
    /// The lease grant verifies the pod's effective release matches the
    /// admission-pinned release and records its manifest digest, write-once per
    /// claim attempt. A component with no injected release identity records no
    /// digest. The caller passes the mounted release's exact package-id set;
    /// selection remains one ordered SQL turn across that whole set.
    async fn claim_next(
        &self,
        component_id: &str,
        package_ids: &[String],
        environment: &str,
        lease_ttl_ms: i64,
    ) -> Result<ProductionClaimResult, ProductionClaimError> {
        if !valid_package_scope(package_ids) || environment.is_empty() || lease_ttl_ms <= 0 {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "validate queue scope",
                "a nonempty canonical package set, environment, and positive lease TTL are required",
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
        let user_id = self.user_id_for(component_id);
        let operation = self.operation_for(component_id);
        let (connection, policy) = self
            .checkout_platform(&project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| {
                ProductionClaimError::new(
                    ProductionClaimErrorKind::Storage,
                    "checkout project connection",
                    format!("{error:?}"),
                )
            })?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                &tenant,
                schema.as_deref(),
                Some(&runner),
                None,
                user_id.as_deref(),
                operation.as_deref(),
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
            package_ids,
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
    /// never from PostgreSQL's non-canonical `jsonb::text` rendering. The
    /// mounted release's exact package-id set is filtered in the same single
    /// global-FIFO turn as ordinary claims.
    async fn reap_exhausted(
        &self,
        component_id: &str,
        package_ids: &[String],
        environment: &str,
        grace_ms: i64,
    ) -> Result<ProductionReapResult, ProductionClaimError> {
        if !valid_package_scope(package_ids) || environment.is_empty() || grace_ms < 0 {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "validate janitor scope",
                "a nonempty canonical package set, environment, and non-negative janitor grace are required",
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
        let user_id = self.user_id_for(component_id);
        let operation = self.operation_for(component_id);
        let (connection, policy) = self
            .checkout_platform(&project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| {
                ProductionClaimError::new(
                    ProductionClaimErrorKind::Storage,
                    "checkout janitor connection",
                    format!("{error:?}"),
                )
            })?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                &tenant,
                schema.as_deref(),
                Some(&runner),
                None,
                user_id.as_deref(),
                operation.as_deref(),
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

        let result = reap_in_transaction(&connection, package_ids, environment, grace_ms).await;
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
    async fn renew(
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
        let user_id = self.user_id_for(component_id);
        let operation = self.operation_for(component_id);
        let (connection, policy) = self
            .checkout_platform(&project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| {
                ProductionClaimError::new(
                    ProductionClaimErrorKind::Storage,
                    "checkout renewal connection",
                    format!("{error:?}"),
                )
            })?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                &tenant,
                schema.as_deref(),
                Some(&runner),
                None,
                user_id.as_deref(),
                operation.as_deref(),
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
    async fn complete(
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
        let user_id = self.user_id_for(component_id);
        let operation = self.operation_for(component_id);
        let (connection, policy) = self
            .checkout_platform(&project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| {
                ProductionClaimError::new(
                    ProductionClaimErrorKind::Storage,
                    "checkout completion connection",
                    format!("{error:?}"),
                )
            })?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                &tenant,
                schema.as_deref(),
                Some(&runner),
                None,
                user_id.as_deref(),
                operation.as_deref(),
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

    /// Store deadline changes before settlement, including attempts that need replay.
    /// Returns false when the run no longer belongs to this lease.
    async fn record_deadline_adjustments(
        &self,
        component_id: &str,
        run_id: &str,
        lease_generation: i64,
        adjustments: &serde_json::Value,
    ) -> Result<bool, ProductionClaimError> {
        if lease_generation <= 0 {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "validate production deadline report",
                "lease generation must be positive",
            ));
        }
        let tenant = self.tenant_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve deadline report tenant",
                "component has no host-injected tenant",
            )
        })?;
        let runner = self.runner_for(component_id).ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Identity,
                "resolve deadline report runner",
                "component has no host-injected runner",
            )
        })?;
        let project = self.project_for(component_id);
        let schema = self.schema_for(component_id);
        let user_id = self.user_id_for(component_id);
        let operation = self.operation_for(component_id);
        let (connection, policy) = self
            .checkout_platform(&project, AuthorityClass::ExecutorPlatform)
            .await
            .map_err(|error| {
                ProductionClaimError::new(
                    ProductionClaimErrorKind::Storage,
                    "checkout deadline report connection",
                    format!("{error:?}"),
                )
            })?;
        if let Err(error) = self
            .begin_with_claims(
                &connection,
                AuthorityClass::ExecutorPlatform,
                &tenant,
                schema.as_deref(),
                Some(&runner),
                None,
                user_id.as_deref(),
                operation.as_deref(),
                None,
                policy.statement_timeout_ms,
            )
            .await
        {
            self.destroy(connection);
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Storage,
                "begin deadline report transaction",
                format!("{error:?}"),
            ));
        }
        let result = async {
            require_executor_authority(&connection).await?;
            let sql = wamn_run_state::transitions::record_deadline_adjustments_sql();
            let statement = connection
                .prepare_cached(&sql)
                .await
                .map_err(|error| storage("prepare deadline report", &error))?;
            let json = adjustments.to_string();
            let row = connection
                .query_one(
                    &statement,
                    &[&run_id, &run_id, &runner, &lease_generation, &json],
                )
                .await
                .map_err(|error| storage("store deadline report", &error))?;
            Ok(row.get::<_, bool>(0))
        }
        .await;
        finish_queue_transaction(self, connection, result, "commit deadline report").await
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
                return Err(storage(commit_operation, &error));
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

async fn require_executor_authority(connection: &Object) -> Result<(), ProductionClaimError> {
    let row = connection
        .query_one(
            CURRENT_USER_ROLE_MEMBERSHIP_SQL,
            &[&AuthorityClass::ExecutorPlatform.acl_role()],
        )
        .await
        .map_err(|error| storage("read executor authority", &error))?;
    let allowed: bool = row_value(&row, 0, "executor authority membership")?;
    if !allowed {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Identity,
            "check executor authority",
            "executor-platform-authority-required",
        ));
    }
    Ok(())
}

async fn renew_in_transaction(
    connection: &Object,
    run_id: &str,
    runner: &str,
    lease_generation: i64,
    lease_ttl_ms: i64,
) -> Result<ProductionLeaseRenewal, ProductionClaimError> {
    require_executor_authority(connection).await?;
    let sql = renew_production_lease_sql();
    let statement = connection
        .prepare_cached(&sql)
        .await
        .map_err(|error| storage("prepare production lease renewal", &error))?;
    let renewed = connection
        .query_opt(
            &statement,
            &[&run_id, &runner, &lease_generation, &lease_ttl_ms],
        )
        .await
        .map_err(|error| storage("renew production lease", &error))?;
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
    require_executor_authority(connection).await?;
    // D2b: the transitions take their instant from this host, not from the
    // database server. One instant serves the caller release and the
    // terminalization, because both statements run inside this one transaction
    // and the server `now()` they replace was the transaction timestamp.
    let completed_at = SystemTime::now();
    if let Some(caller) = completion.caller() {
        let body_json = serde_json::to_string(caller.body()).map_err(|error| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "serialize production caller outcome",
                error.to_string(),
            )
        })?;
        let hash = wamn_execution_contract::canonical_json_sha256(caller.body());
        let http_status = i32::from(caller.http_status());
        let release_node_id = caller.release_node_id();
        let sql = release_caller_sql();
        let statement = connection
            .prepare_cached(&sql)
            .await
            .map_err(|error| storage("prepare production caller release", &error))?;
        let row = connection
            .query_one(
                &statement,
                &[
                    &run_id,
                    &run_id,
                    &runner,
                    &lease_generation,
                    &caller.kind(),
                    &body_json,
                    &http_status,
                    &release_node_id,
                    &hash,
                    &completed_at,
                ],
            )
            .await
            .map_err(|error| storage("release production caller", &error))?;
        let release = decode_caller_release(&row)?;
        match release {
            CallerReleaseResult::Released => {}
            CallerReleaseResult::AlreadyReleased(stored)
                if stored.exactly_matches(
                    caller.kind(),
                    caller.body(),
                    Some(caller.http_status()),
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

    let result_json = serde_json::to_string(completion.result()).map_err(|error| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "serialize production result",
            error.to_string(),
        )
    })?;
    let fail_kind = completion.fail_kind().map(FailKind::as_sql);
    let sql = terminalize_sql();
    let statement = connection
        .prepare_cached(&sql)
        .await
        .map_err(|error| storage("prepare production terminalization", &error))?;
    let row = connection
        .query_one(
            &statement,
            &[
                &run_id,
                &run_id,
                &runner,
                &lease_generation,
                &completion.status().as_sql(),
                &completion.terminal_reason(),
                &result_json,
                &fail_kind,
                &completed_at,
            ],
        )
        .await
        .map_err(|error| storage("terminalize production run", &error))?;
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
    package_ids: &[String],
    environment: &str,
    lease_ttl_ms: i64,
    release: Option<&ReleaseIdentity>,
) -> Result<ClaimTurn, ProductionClaimError> {
    require_executor_authority(connection).await?;
    let select_sql = select_production_claim_sql();
    let select = connection
        .prepare_cached(&select_sql)
        .await
        .map_err(|error| storage("prepare production candidate", &error))?;
    let Some(row) = connection
        .query_opt(&select, &[&package_ids, &environment])
        .await
        .map_err(|error| storage("select production candidate", &error))?
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
            .map_err(|error| storage("prepare effect-attempt classification", &error))?;
        let effect_row = connection
            .query_one(&effect_statement, &[&selected.run_id])
            .await
            .map_err(|error| storage("classify effect-attempt evidence", &error))?;
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
                .map_err(|error| storage("prepare pre-effect state clear", &error))?;
            connection
                .query_one(&clear, &[&selected.run_id])
                .await
                .map_err(|error| storage("clear pre-effect state", &error))?;
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
        .map_err(|error| storage("prepare crash-evidence advance", &error))?;
    connection
        .query_one(&advance, &[&selected.run_id])
        .await
        .map_err(|error| storage("advance crash evidence", &error))?;

    let grant_sql = grant_production_claim_sql();
    let grant = connection
        .prepare_cached(&grant_sql)
        .await
        .map_err(|error| storage("prepare production lease grant", &error))?;
    // The pod's own release identity, or NULL for both when it carries none.
    // PostgreSQL compares the effective release id to the immutable admission
    // pin and records only the digest.
    let release = release.filter(|_| selected.candidate.is_none());
    let effective_release_id: Option<i32> = release.map(|identity| identity.effective_release_id);
    let manifest_digest: Option<&str> = release.map(|identity| identity.manifest_digest.as_str());
    // The grant is the one abortable write left in this transaction, so it runs
    // in its own subtransaction: a database refusal rolls back to the savepoint
    // instead of the whole transaction, leaving the advance above committable.
    connection
        .batch_execute("SAVEPOINT wamn_production_grant")
        .await
        .map_err(|error| storage("open production lease savepoint", &error))?;
    let granted = connection
        .query_opt(
            &grant,
            &[
                &selected.run_id,
                &runner,
                &lease_ttl_ms,
                &effective_release_id,
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
                .map_err(|rollback| storage("roll back refused production lease", &rollback))?;
            return Ok(ClaimTurn::GrantRefused(storage(
                "grant production lease",
                &error,
            )));
        }
    };
    let row = granted.ok_or_else(|| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "grant production lease",
            "claiming effective release does not match the run admission pin",
        )
    })?;
    let lease_generation = row_value(&row, 0, "lease generation")?;
    Ok(ClaimTurn::Claimed(ProductionClaimResult::Ready {
        run_id: selected.run_id,
        package_id: selected.package_id,
        payload: selected.payload,
        lease_generation,
        wiring_id: selected.wiring_id,
        wiring_version: selected.wiring_version,
        router_caller_attached: selected.router_caller_attached,
        durable_caller_attached: selected.durable_caller_attached,
        candidate: selected.candidate,
        service_principal_id: selected.service_principal_id,
    }))
}

async fn reap_in_transaction(
    connection: &Object,
    package_ids: &[String],
    environment: &str,
    grace_ms: i64,
) -> Result<ProductionReapResult, ProductionClaimError> {
    require_executor_authority(connection).await?;
    let select_sql = select_exhausted_production_sql();
    let select = connection
        .prepare_cached(&select_sql)
        .await
        .map_err(|error| storage("prepare exhausted candidate", &error))?;
    let Some(row) = connection
        .query_opt(&select, &[&grace_ms, &package_ids, &environment])
        .await
        .map_err(|error| storage("select exhausted candidate", &error))?
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
            .map_err(|error| storage("prepare exhausted effect classification", &error))?;
        let row = connection
            .query_one(&effect, &[&selected.run_id])
            .await
            .map_err(|error| storage("classify exhausted effect evidence", &error))?;
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
        .map_err(|error| storage("prepare exhausted terminalization", &error))?;
    // D2b: this host owns the instant the reap stamps.
    let reaped_at = SystemTime::now();
    let row = connection
        .query_opt(
            &terminalize,
            &[&selected.run_id, &body, &body_hash, &reaped_at],
        )
        .await
        .map_err(|error| storage("terminalize exhausted run", &error))?
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
        .map_err(|error| storage("prepare effect-intent fence", &error))?;
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
        .map_err(|error| storage("prepare effect-uncertain terminalization", &error))?;
    // D2b: this host owns the instant the effect-uncertain hand-off stamps.
    let uncertain_at = SystemTime::now();
    let row = connection
        .query_opt(&statement, &[&selected.run_id, &body, &hash, &uncertain_at])
        .await
        .map_err(|error| storage("terminalize effect uncertainty", &error))?
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
    let effective_release_id: i32 = row_value(row, 11, "effective release id")?;
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
    let service_principal_id: Option<String> = row_value(row, 15, "service principal")?;
    let candidate = match (flow_id, flow_version, wiring_hash, binding_world) {
        (None, None, Some(hash), None) if service_principal_id.is_some() && !hash.is_empty() => {
            None
        }
        (Some(flow_id), Some(flow_version), None, None)
            if !flow_id.is_empty() && flow_version > 0 =>
        {
            None
        }
        (None, None, Some(wiring_hash), Some(binding_world))
            if effective_release_id > 0 && !wiring_hash.is_empty() =>
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
                effective_release_id,
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
        package_id: row_value(row, 14, "package id")?,
        had_prior_lease: row_value(row, 1, "prior lease evidence")?,
        status,
        payload,
        durability_class: DurabilityClass::from_sql_or_default(&class_text),
        wiring_id,
        wiring_version,
        router_caller_attached,
        durable_caller_attached,
        candidate,
        service_principal_id,
    })
}

fn valid_package_scope(package_ids: &[String]) -> bool {
    !package_ids.is_empty()
        && package_ids.iter().all(|package_id| {
            !package_id.is_empty()
                && package_id.trim() == package_id
                && !package_id.as_bytes().contains(&0)
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

/// Preserve the database message and constraint, without row-bearing DETAIL or HINT.
/// Run rows contain application inputs, results, and invocation context.
fn storage(operation: &'static str, error: &tokio_postgres::Error) -> ProductionClaimError {
    let detail = match error.as_db_error() {
        Some(database) => match database.constraint() {
            Some(constraint) => format!("{} ({constraint})", database.message()),
            None => database.message().to_owned(),
        },
        None => error.to_string(),
    };
    ProductionClaimError::new(ProductionClaimErrorKind::Storage, operation, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn database_diagnostics_omit_row_detail_and_hint() -> anyhow::Result<()> {
        let _lock = wamn_test_postgres::lock();
        let database = wamn_test_postgres::database();
        let (client, connection) =
            tokio_postgres::connect(database.url(), tokio_postgres::NoTls).await?;
        let task = tokio::spawn(connection);
        client.batch_execute("CREATE TEMP TABLE private_diagnostic (payload text, valid bool CONSTRAINT diagnostic_valid CHECK (valid))").await?;
        let marker = "private-person@example.invalid";
        let error = client
            .execute(
                "INSERT INTO private_diagnostic VALUES ($1, false)",
                &[&marker],
            )
            .await
            .unwrap_err();
        assert!(
            error
                .as_db_error()
                .unwrap()
                .detail()
                .unwrap()
                .contains(marker)
        );
        let rendered = storage("test storage", &error).to_string();
        assert!(rendered.contains("diagnostic_valid"));
        assert!(rendered.contains("violates check constraint"));
        assert!(!rendered.contains(marker));
        let error = client.batch_execute("DO $$ BEGIN RAISE EXCEPTION 'diagnostic message' USING DETAIL = 'private-detail-marker', HINT = 'private-hint-marker'; END $$").await.unwrap_err();
        let rendered = storage("test storage", &error).to_string();
        assert!(rendered.contains("diagnostic message"));
        assert!(!rendered.contains("private-detail-marker"));
        assert!(!rendered.contains("private-hint-marker"));
        drop(client);
        task.await??;
        Ok(())
    }

    #[tokio::test]
    async fn executor_authority_uses_current_user_for_each_operation() -> anyhow::Result<()> {
        let _lock = wamn_test_postgres::lock();
        let database = wamn_test_postgres::database();
        let config: tokio_postgres::Config = database.url().parse()?;
        let pool = deadpool_postgres::Pool::builder(deadpool_postgres::Manager::new(
            config,
            tokio_postgres::NoTls,
        ))
        .max_size(1)
        .build()?;
        let connection = pool.get().await?;
        connection.batch_execute("BEGIN").await?;
        connection
            .batch_execute(
                "DO $roles$ DECLARE role_name text; BEGIN \
                   FOREACH role_name IN ARRAY ARRAY[ \
                     'wamn_executor_platform', 'wamn_management_admitter', \
                     'wamn_app', 'wamn_control_author', 'wamn_scenario_author', \
                     'wamn_executor_authority_test' \
                   ] LOOP \
                     IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = role_name) THEN \
                       EXECUTE format('CREATE ROLE %I NOSUPERUSER NOBYPASSRLS', role_name); \
                     END IF; \
                   END LOOP; \
                 END $roles$; \
                 GRANT wamn_executor_platform TO wamn_executor_authority_test; \
                 SET LOCAL ROLE wamn_executor_authority_test;",
            )
            .await?;
        require_executor_authority(&connection).await?;
        let users = connection
            .query_one("SELECT CURRENT_USER::text, SESSION_USER::text", &[])
            .await?;
        assert_eq!(users.get::<_, String>(0), "wamn_executor_authority_test");
        assert_ne!(users.get::<_, String>(0), users.get::<_, String>(1));

        let packages = ["authority-test".to_string()];
        let completion = ProductionCompletion::completed(serde_json::Value::Null, None);
        for role in [
            "wamn_management_admitter",
            "wamn_app",
            "wamn_control_author",
            "wamn_scenario_author",
        ] {
            connection
                .batch_execute(&format!("SET LOCAL ROLE {role}"))
                .await?;
            for result in [
                claim_in_transaction(&connection, "runner", &packages, "test", 1000, None)
                    .await
                    .map(|_| ()),
                reap_in_transaction(&connection, &packages, "test", 0)
                    .await
                    .map(|_| ()),
                renew_in_transaction(&connection, "run", "runner", 1, 1000)
                    .await
                    .map(|_| ()),
                complete_in_transaction(&connection, "run", "runner", 1, &completion)
                    .await
                    .map(|_| ()),
            ] {
                let error = result.expect_err("a different role must not enter executor work");
                assert_eq!(error.kind(), ProductionClaimErrorKind::Identity);
                assert_eq!(error.operation(), "check executor authority");
                assert!(
                    error
                        .to_string()
                        .contains("executor-platform-authority-required")
                );
            }
            assert_eq!(
                connection
                    .query_one("SELECT 1", &[])
                    .await?
                    .get::<_, i32>(0),
                1
            );
        }
        connection
            .batch_execute("RESET ROLE; SAVEPOINT missing_role")
            .await?;
        connection
            .batch_execute(&format!(
                "ALTER ROLE wamn_executor_platform RENAME TO wamn_authority_missing_{}",
                std::process::id()
            ))
            .await?;
        let missing = require_executor_authority(&connection)
            .await
            .expect_err("an absent role must return the native refusal");
        assert_eq!(missing.kind(), ProductionClaimErrorKind::Identity);
        assert!(
            missing
                .to_string()
                .contains("executor-platform-authority-required")
        );
        connection
            .batch_execute("ROLLBACK TO missing_role; ROLLBACK")
            .await?;
        Ok(())
    }

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
    fn lease_grant_uses_a_fresh_post_fence_clock() {
        let lease_sql = grant_production_claim_sql();
        assert!(lease_sql.contains("lease_expires_at = statement_timestamp()"));
        assert!(!lease_sql.contains("lease_expires_at = now()"));
    }

    #[test]
    fn lease_grant_verifies_release_and_mints_manifest_on_the_existing_write() {
        let lease_sql = grant_production_claim_sql();
        for required in [
            "SET status = 'running'",
            "manifest_digest = $5",
            "r.effective_release_id = $4",
            "r.status IN ('dispatched', 'running')",
            "FROM leased JOIN marked",
        ] {
            assert!(lease_sql.contains(required), "lease grant omits {required}");
        }
        assert_eq!(
            lease_sql.matches("UPDATE runs").count(),
            1,
            "the record is minted on the existing claim write, not a second one"
        );

        assert!(!lease_sql.contains("release_version"));

        // The digest travels from the claiming pod, so the candidate select
        // never reads it back; a decoder that grew a field would need this to
        // change.
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
            "r.effective_release_id",
            "r.wiring_hash",
            "r.binding_world_json::text",
            "r.package_id",
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
    fn package_scope_requires_at_least_one_canonical_identity() {
        assert!(!valid_package_scope(&[]));
        assert!(!valid_package_scope(&[String::new()]));
        assert!(!valid_package_scope(&[" fixture".to_owned()]));
        assert!(!valid_package_scope(&["fixture\0overlay".to_owned()]));
        assert!(valid_package_scope(&[
            "base".to_owned(),
            "client_overlay".to_owned(),
        ]));
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
