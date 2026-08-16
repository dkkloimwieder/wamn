//! Host-only composition of one production queue claim transaction.

use std::fmt::{Display, Formatter};

use deadpool_postgres::Object;
use tokio_postgres::Row;
use tokio_postgres::types::FromSql;
use wamn_run_state::queue::{
    ProductionClaimClass, classify_production_claim, clear_pre_effect_state_sql,
    grant_production_claim_sql, select_claim_effect_attempt_sql, select_exhausted_production_sql,
    select_pre_effect_projection_sql, select_production_claim_sql, serialize_effect_intent_sql,
    terminalize_effect_uncertain_claim_sql, terminalize_exhausted_production_sql,
};
use wamn_run_state::{EffectUncertainFailure, FailKind, RunStatus};

use super::WamnPostgres;

/// Stable category for a production-claim failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductionClaimErrorKind {
    /// Required host-injected identity was absent.
    Identity,
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
    /// A fresh lease committed for guest execution.
    Ready {
        run_id: String,
        payload: String,
        lease_generation: i64,
    },
    /// A committed app claim observed an exact expired pre-effect fence whose
    /// mutable projection must be cleared by the private native writer.
    ResetRequired {
        run_id: String,
        prior_lease_owner: String,
        prior_lease_expires_at: String,
        prior_lease_generation: i64,
    },
    /// Claim-time classification removed the row without execution.
    Terminalized {
        run_id: String,
        status: RunStatus,
        fail_kind: FailKind,
    },
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

#[derive(Debug)]
struct SelectedClaim {
    run_id: String,
    had_prior_lease: bool,
    prior_lease_owner: Option<String>,
    prior_lease_expires_at: Option<String>,
    prior_lease_generation: i64,
    status: RunStatus,
    payload: String,
}

#[derive(Debug)]
struct ExhaustedClaim {
    run_id: String,
    status: RunStatus,
    root_flow_id: String,
    flow_version: i32,
}

impl WamnPostgres {
    /// Lock, classify, and lease at most one production run.
    ///
    /// Tenant, project, schema, and lease owner come only from host-injected
    /// component identity. A `Ready` result is returned only after COMMIT, so
    /// the guest's separate plan-supply transactions can observe the committed
    /// lease.
    pub async fn claim_next_production(
        &self,
        component_id: &str,
        lease_ttl_ms: i64,
    ) -> Result<ProductionClaimResult, ProductionClaimError> {
        if lease_ttl_ms <= 0 {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "validate lease TTL",
                "lease TTL must be positive",
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
        let project = self.project_for(component_id);
        let schema = self.schema_for(component_id);
        let (connection, policy) = self.checkout(&project).await.map_err(|error| {
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

        let result = claim_in_transaction(&connection, &runner, lease_ttl_ms).await;
        match result {
            Ok(result) => {
                if let Err(error) = connection.batch_execute("COMMIT").await {
                    self.destroy(connection);
                    return Err(ProductionClaimError::new(
                        ProductionClaimErrorKind::Storage,
                        "commit production claim",
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
        grace_ms: i64,
    ) -> Result<ProductionReapResult, ProductionClaimError> {
        if grace_ms < 0 {
            return Err(ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "validate janitor grace",
                "janitor grace must not be negative",
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
        let (connection, policy) = self.checkout(&project).await.map_err(|error| {
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

        let result = reap_in_transaction(&connection, grace_ms).await;
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
}

async fn claim_in_transaction(
    connection: &Object,
    runner: &str,
    lease_ttl_ms: i64,
) -> Result<ProductionClaimResult, ProductionClaimError> {
    let select_sql = select_production_claim_sql();
    let select = connection
        .prepare_cached(&select_sql)
        .await
        .map_err(|error| storage("prepare production candidate", error))?;
    let Some(row) = connection
        .query_opt(&select, &[])
        .await
        .map_err(|error| storage("select production candidate", error))?
    else {
        return Ok(ProductionClaimResult::Empty);
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
    let has_effect_attempt = row_value(&effect_row, 0, "effect-attempt evidence")?;

    match classify_production_claim(selected.had_prior_lease, has_effect_attempt) {
        ProductionClaimClass::ExpiredWithAttempt => {
            return terminalize_effect_uncertain(connection, &selected).await;
        }
        ProductionClaimClass::ExpiredPreEffect => {
            let projection_sql = select_pre_effect_projection_sql();
            let projection = connection
                .prepare_cached(&projection_sql)
                .await
                .map_err(|error| storage("prepare pre-effect projection check", error))?;
            let projection_row = connection
                .query_one(&projection, &[&selected.run_id])
                .await
                .map_err(|error| storage("classify pre-effect projection", error))?;
            let has_projection = row_value(&projection_row, 0, "pre-effect projection")?;
            if has_projection {
                return Ok(ProductionClaimResult::ResetRequired {
                    run_id: selected.run_id,
                    prior_lease_owner: selected.prior_lease_owner.ok_or_else(|| {
                        ProductionClaimError::new(
                            ProductionClaimErrorKind::Contract,
                            "decode reset fence",
                            "expired claim lacks prior lease owner",
                        )
                    })?,
                    prior_lease_expires_at: selected.prior_lease_expires_at.ok_or_else(|| {
                        ProductionClaimError::new(
                            ProductionClaimErrorKind::Contract,
                            "decode reset fence",
                            "expired claim lacks prior lease expiry",
                        )
                    })?,
                    prior_lease_generation: selected.prior_lease_generation,
                });
            }
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
        ProductionClaimClass::Ordinary => {}
    }

    let grant_sql = grant_production_claim_sql();
    let grant = connection
        .prepare_cached(&grant_sql)
        .await
        .map_err(|error| storage("prepare production lease grant", error))?;
    let row = connection
        .query_opt(&grant, &[&selected.run_id, &runner, &lease_ttl_ms])
        .await
        .map_err(|error| storage("grant production lease", error))?
        .ok_or_else(|| {
            ProductionClaimError::new(
                ProductionClaimErrorKind::Contract,
                "grant production lease",
                "selected queue row disappeared while locked",
            )
        })?;
    let lease_generation = row_value(&row, 0, "lease generation")?;
    Ok(ProductionClaimResult::Ready {
        run_id: selected.run_id,
        payload: selected.payload,
        lease_generation,
    })
}

async fn reap_in_transaction(
    connection: &Object,
    grace_ms: i64,
) -> Result<ProductionReapResult, ProductionClaimError> {
    let select_sql = select_exhausted_production_sql();
    let select = connection
        .prepare_cached(&select_sql)
        .await
        .map_err(|error| storage("prepare exhausted candidate", error))?;
    let Some(row) = connection
        .query_opt(&select, &[&grace_ms])
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
    let selected = ExhaustedClaim {
        run_id: row_value(&row, 1, "exhausted run id")?,
        status,
        root_flow_id: row_value(&row, 3, "exhausted root flow id")?,
        flow_version: row_value(&row, 4, "exhausted root flow version")?,
    };
    if !matches!(selected.status, RunStatus::Dispatched | RunStatus::Running) {
        return Err(ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "validate exhausted candidate",
            format!("non-runnable status {}", selected.status.as_sql()),
        ));
    }

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

    let (body, body_hash) = generic_failure_outcome(
        "infrastructure-failure",
        &selected.root_flow_id,
        selected.flow_version,
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
    root_flow_id: &str,
    flow_version: i32,
    run_id: &str,
) -> Result<(String, String), ProductionClaimError> {
    let flow_version = u32::try_from(flow_version).map_err(|_| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "build generic failure outcome",
            format!("invalid root flow version {flow_version}"),
        )
    })?;
    let body = serde_json::json!({
        "error": {
            "code": code,
            "flow-id": root_flow_id,
            "flow-version": flow_version,
            "run-id": run_id,
        }
    });
    let body_hash = wamn_flow::canonical_json_sha256(&body);
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
    let status: String = row_value(row, 6, "run status")?;
    let status = RunStatus::from_sql(&status).ok_or_else(|| {
        ProductionClaimError::new(
            ProductionClaimErrorKind::Contract,
            "decode production candidate",
            format!("unknown run status {status:?}"),
        )
    })?;
    Ok(SelectedClaim {
        run_id: row_value(row, 1, "run id")?,
        had_prior_lease: row_value(row, 2, "prior lease evidence")?,
        prior_lease_owner: row_value(row, 3, "prior lease owner")?,
        prior_lease_expires_at: row_value(row, 4, "prior lease expiry")?,
        prior_lease_generation: row_value(row, 5, "prior lease generation")?,
        status,
        payload: row_value(row, 13, "authoritative input")?,
    })
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

fn storage(operation: &'static str, error: tokio_postgres::Error) -> ProductionClaimError {
    ProductionClaimError::new(
        ProductionClaimErrorKind::Storage,
        operation,
        error.to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_refusal_body_is_exact_and_message_free() {
        let (json, hash) = generic_failure_outcome("foreign-revision", "root", 7, "run-1").unwrap();
        let body = serde_json::from_str::<serde_json::Value>(&json).unwrap();
        assert_eq!(
            body,
            serde_json::from_str::<serde_json::Value>(
                r#"{"error":{"code":"foreign-revision","flow-id":"root","flow-version":7,"run-id":"run-1"}}"#,
            )
            .unwrap()
        );
        assert!(body["error"].get("message").is_none());
        assert_eq!(hash, wamn_flow::canonical_json_sha256(&body));
    }

    #[test]
    fn janitor_failure_body_uses_host_jcs_not_database_json_text() {
        let (json, hash) =
            generic_failure_outcome("infrastructure-failure", "root-flow", 19, "run-exhausted")
                .unwrap();
        assert_eq!(
            json,
            r#"{"error":{"code":"infrastructure-failure","flow-id":"root-flow","flow-version":19,"run-id":"run-exhausted"}}"#
        );
        let body = serde_json::from_str(&json).unwrap();
        assert_eq!(hash, wamn_flow::canonical_json_sha256(&body));
    }

    #[test]
    fn claim_is_exactly_lock_then_classify_then_lease() {
        let source = include_str!("production_claim.rs");
        let start = source.find("async fn claim_in_transaction(").unwrap();
        let end = source.find("async fn reap_in_transaction(").unwrap();
        let body = &source[start..end];
        let lock = body.find("select_production_claim_sql()").unwrap();
        let classify = body.find("classify_production_claim(").unwrap();
        let lease = body.find("grant_production_claim_sql()").unwrap();
        assert!(lock < classify && classify < lease);
        for removed in [
            "load_candidate_override",
            "load_release_plans",
            "load_release_bindings",
            "terminalize_refusal",
        ] {
            assert!(!body.contains(removed), "claim retains {removed}");
        }
    }

    #[test]
    fn claim_and_reaper_fence_before_fresh_effect_snapshot() {
        let source = include_str!("production_claim.rs");
        let claim_start = source.find("async fn claim_in_transaction(").unwrap();
        let reap_start = source.find("async fn reap_in_transaction(").unwrap();
        let fence_helper = source.find("async fn serialize_effect_intent(").unwrap();
        for body in [
            &source[claim_start..reap_start],
            &source[reap_start..fence_helper],
        ] {
            let fence = body
                .find("serialize_effect_intent(connection")
                .expect("composer acquires the shared fence");
            let snapshot = body
                .find("let effect_sql = select_claim_effect_attempt_sql()")
                .expect("composer reads a fresh effect snapshot");
            assert!(fence < snapshot);
        }
        let tests_start = source.find("#[cfg(test)]").unwrap();
        let helper = &source[fence_helper..tests_start];
        assert!(helper.contains("let sql = serialize_effect_intent_sql();"));
        assert!(!helper.contains("SELECT true"));
    }

    #[test]
    fn lease_grant_uses_a_fresh_post_fence_clock() {
        let lease_sql = grant_production_claim_sql();
        assert!(lease_sql.contains("lease_expires_at = statement_timestamp()"));
        assert!(!lease_sql.contains("lease_expires_at = now()"));
    }

    #[test]
    fn expired_projection_handoff_commits_before_private_delete_and_reclaims_fresh() {
        let source = include_str!("production_claim.rs");
        let start = source.find("async fn claim_in_transaction(").unwrap();
        let end = source.find("async fn reap_in_transaction(").unwrap();
        let body = &source[start..end];
        let classify = body.find("ProductionClaimClass::ExpiredPreEffect").unwrap();
        let projection = body.find("select_pre_effect_projection_sql()").unwrap();
        let handoff = body.find("ProductionClaimResult::ResetRequired").unwrap();
        let clear = body.find("clear_pre_effect_state_sql()").unwrap();
        let grant = body.find("grant_production_claim_sql()").unwrap();
        assert!(classify < projection && projection < handoff);
        assert!(handoff < clear && clear < grant);
        assert!(!body.contains("DELETE FROM node_runs"));
    }
}
