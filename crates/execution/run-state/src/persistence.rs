//! Run persistence over a connection authorized by the caller.

use crate::queue::{
    advance_lease_attempts_sql, clear_pre_effect_state_sql, grant_production_lease_sql,
    release_production_lease_sql, renew_production_lease_sql, select_exhausted_production_sql,
    select_production_lease_sql, terminalize_exhausted_production_sql,
    terminalize_released_production_sql,
};
use crate::run_store::{
    ProductionCompletion, ProductionCompletionResult, ProductionLeaseError,
    ProductionLeaseErrorType, ProductionLeaseRenewal, ProductionReapResult,
};
use crate::transitions::{TerminalizeResult, terminalize_sql};
use crate::{FailType, RunStatus};
use sqlx::postgres::PgRow as Row;
use sqlx::{Decode, Row as _, Type};
use std::time::SystemTime;

/// Result of one host-only production queue turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductionLeaseResult {
    /// No eligible row was visible to this project.
    Empty,
    /// A fresh lease for router execution after the caller commits.
    Ready {
        run_id: String,
        package_id: String,
        payload: serde_json::Value,
        lease_generation: i64,
        wiring_id: String,
        wiring_version: i32,
        workflow: Box<ProductionWorkflow>,
    },
}

/// What an event, manual or schedule workflow run froze at admission
/// (docs/architecture/execution.md#workflows): the graph hash, the principal it
/// executes as, its one active role, and the component digest of every node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductionWorkflow {
    pub wiring_hash: String,
    pub execution_principal_id: String,
    pub execution_role: String,
    pub node_digests: std::collections::BTreeMap<String, String>,
    pub causation: Option<wamn_event_wire::Causation>,
}

/// The composed outcome of one lease transaction, decided before COMMIT.
#[derive(Debug)]
pub enum LeaseTurn {
    /// The transaction may commit and report this result.
    Leased(ProductionLeaseResult),
    /// The lease grant was refused inside its own subtransaction, which the
    /// composer already rolled back. The transaction is still live and MUST
    /// commit so the crash-evidence attempt advance taken before the savepoint
    /// survives — a refusal that rolls its own attempt counter back can never
    /// reach `max_attempts`, so the janitor can never reap the run and it stays
    /// the project's FIFO head forever (wamn-0h0g.15.69). The lease itself still
    /// fails with this error.
    GrantRefused(ProductionLeaseError),
}

#[derive(Debug)]
struct SelectedLease {
    run_id: String,
    package_id: String,
    had_prior_lease: bool,
    status: RunStatus,
    payload: serde_json::Value,
    wiring_id: String,
    wiring_version: i32,
    workflow: Box<ProductionWorkflow>,
}

#[derive(Debug)]
struct ExhaustedLease {
    run_id: String,
    status: RunStatus,
    identity: ExhaustedExecutionIdentity,
}

#[derive(Debug)]
struct ExhaustedExecutionIdentity {
    wiring_id: String,
    wiring_version: i32,
}

/// Release a held lease or fail the run when its retry budget is spent.
pub async fn release_lease(
    connection: &mut sqlx::PgConnection,
    run_id: &str,
    runner: &str,
    lease_generation: i64,
) -> Result<bool, ProductionLeaseError> {
    validate_generation(lease_generation)?;
    let sql = release_production_lease_sql();
    let Some(row) = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(run_id)
        .bind(runner)
        .bind(lease_generation)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("release production lease", &error))?
    else {
        return Ok(false);
    };
    if row_value::<bool>(&row, 0, "released run exhausted")? {
        let identity = ExhaustedExecutionIdentity {
            wiring_id: row_value(&row, 1, "released wiring id")?,
            wiring_version: row_value(&row, 2, "released wiring version")?,
        };
        let body = generic_failure_outcome(FailType::RetryExhausted.as_sql(), &identity, run_id);
        let sql = terminalize_released_production_sql();
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(run_id)
            .bind(&body)
            .bind(chrono::Utc::now())
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| {
                ProductionLeaseError::postgres("terminalize release exhaustion", &error)
            })?;
    }
    Ok(true)
}

/// Renew a live lease under its owner and generation fence.
pub async fn renew(
    connection: &mut sqlx::PgConnection,
    run_id: &str,
    runner: &str,
    lease_generation: i64,
    lease_ttl_ms: i64,
) -> Result<ProductionLeaseRenewal, ProductionLeaseError> {
    validate_generation(lease_generation)?;
    validate_duration(lease_ttl_ms)?;
    let sql = renew_production_lease_sql();
    let renewed = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(run_id)
        .bind(runner)
        .bind(lease_generation)
        .bind(lease_ttl_ms)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("renew production lease", &error))?;
    Ok(if renewed.is_some() {
        ProductionLeaseRenewal::Renewed
    } else {
        ProductionLeaseRenewal::FenceLost
    })
}

/// Store the terminal outcome and remove the queue entry under the lease fence.
pub async fn complete(
    connection: &mut sqlx::PgConnection,
    run_id: &str,
    runner: &str,
    lease_generation: i64,
    completion: &ProductionCompletion,
) -> Result<ProductionCompletionResult, ProductionLeaseError> {
    validate_generation(lease_generation)?;
    // D2b: the transitions take their instant from this host, not from the
    // database server. Settlement stamps the instant chosen by this host.
    let completed_at = SystemTime::now();
    let result_json = completion.result().to_string();
    let fail_type = completion.fail_type().map(FailType::as_sql);
    let sql = terminalize_sql();
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(run_id)
        .bind(run_id)
        .bind(runner)
        .bind(lease_generation)
        .bind(completion.status().as_sql())
        .bind(completion.terminal_reason())
        .bind(&result_json)
        .bind(fail_type)
        .bind(chrono::DateTime::<chrono::Utc>::from(completed_at))
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("terminalize production run", &error))?;
    match decode_terminalization(&row)? {
        TerminalizeResult::Terminalized => Ok(ProductionCompletionResult::Terminalized),
        TerminalizeResult::RunTerminal(status) => {
            Ok(ProductionCompletionResult::AlreadyTerminal(status))
        }
        TerminalizeResult::FenceLost => Ok(ProductionCompletionResult::FenceLost),
        TerminalizeResult::NotFound => Ok(ProductionCompletionResult::NotFound),
        TerminalizeResult::CrossRunAuthority => Err(ProductionLeaseError::new(
            ProductionLeaseErrorType::Contract,
            "terminalize production run",
            "production-cross-run-authority",
        )),
    }
}

fn decode_terminalization(row: &Row) -> Result<TerminalizeResult, ProductionLeaseError> {
    let code: String = row_value(row, 0, "terminalization result")?;
    let status: Option<String> = row_value(row, 1, "terminalization run status")?;
    TerminalizeResult::from_parts(&code, status.as_deref().unwrap_or_default()).ok_or_else(|| {
        ProductionLeaseError::new(
            ProductionLeaseErrorType::Contract,
            "decode production terminalization",
            format!("unknown or incomplete result {code:?}"),
        )
    })
}

/// Lease one run. The caller commits both `Leased` and `GrantRefused` turns.
pub async fn lease_next(
    connection: &mut sqlx::PgConnection,
    runner: &str,
    package_ids: &[String],
    environment: &str,
    lease_ttl_ms: i64,
    manifest_digest: Option<&str>,
) -> Result<LeaseTurn, ProductionLeaseError> {
    validate_target(package_ids, environment)?;
    validate_duration(lease_ttl_ms)?;
    let select_sql = select_production_lease_sql();
    // The select skips runs pinned to another release, so their head never
    // blocks this host's own backlog (R20). The grant below tests again the pin.
    let Some(row) = sqlx::query(sqlx::AssertSqlSafe(select_sql))
        .bind(package_ids)
        .bind(environment)
        .bind(manifest_digest)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("select production lease", &error))?
    else {
        return Ok(LeaseTurn::Leased(ProductionLeaseResult::Empty));
    };
    let selected = decode_selected_lease(&row)?;
    if !matches!(selected.status, RunStatus::Dispatched | RunStatus::Running) {
        return Err(ProductionLeaseError::new(
            ProductionLeaseErrorType::Contract,
            "validate selected run",
            format!(
                "queue row {} references non-runnable status {}",
                selected.run_id,
                selected.status.as_sql()
            ),
        ));
    }

    if selected.had_prior_lease {
        sqlx::query(sqlx::AssertSqlSafe(clear_pre_effect_state_sql()))
            .bind(&selected.run_id)
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| ProductionLeaseError::postgres("clear pre-effect state", &error))?;
    }

    // Crash evidence advances in its own statement outside the grant's
    // subtransaction.
    let advance_sql = advance_lease_attempts_sql();
    sqlx::query(sqlx::AssertSqlSafe(advance_sql))
        .bind(&selected.run_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("advance crash evidence", &error))?;

    let grant_sql = grant_production_lease_sql();
    // The pod's own release digest, or NULL when it carries none. PostgreSQL
    // compares it to the immutable admission pin.
    // The grant is the one abortable write left in this transaction, so it runs
    // in its own subtransaction: a database refusal rolls back to the savepoint
    // instead of the whole transaction, leaving the advance above committable.
    sqlx::raw_sql("SAVEPOINT wamn_production_grant")
        .execute(&mut *connection)
        .await
        .map(|_| ())
        .map_err(|error| {
            ProductionLeaseError::postgres("open production lease savepoint", &error)
        })?;
    let granted = sqlx::query(sqlx::AssertSqlSafe(grant_sql))
        .bind(&selected.run_id)
        .bind(runner)
        .bind(lease_ttl_ms)
        .bind(manifest_digest)
        .fetch_optional(&mut *connection)
        .await;
    let granted = match granted {
        Ok(granted) => granted,
        Err(error) => {
            sqlx::raw_sql("ROLLBACK TO SAVEPOINT wamn_production_grant")
                .execute(&mut *connection)
                .await
                .map(|_| ())
                .map_err(|rollback| {
                    ProductionLeaseError::postgres("roll back refused production lease", &rollback)
                })?;
            return Ok(LeaseTurn::GrantRefused(ProductionLeaseError::postgres(
                "grant production lease",
                &error,
            )));
        }
    };
    let row = granted.ok_or_else(|| {
        ProductionLeaseError::new(
            ProductionLeaseErrorType::Contract,
            "grant production lease",
            "leasing release digest does not match the run admission pin",
        )
    })?;
    let lease_generation = row_value(&row, 0, "lease generation")?;
    Ok(LeaseTurn::Leased(ProductionLeaseResult::Ready {
        run_id: selected.run_id,
        package_id: selected.package_id,
        payload: selected.payload,
        lease_generation,
        wiring_id: selected.wiring_id,
        wiring_version: selected.wiring_version,
        workflow: selected.workflow,
    }))
}

/// Fail and dequeue one expired run whose retry budget is spent.
pub async fn reap_exhausted(
    connection: &mut sqlx::PgConnection,
    package_ids: &[String],
    environment: &str,
    grace_ms: i64,
) -> Result<ProductionReapResult, ProductionLeaseError> {
    validate_target(package_ids, environment)?;
    if grace_ms < 0 {
        return Err(invalid_parameters("janitor grace must be nonnegative"));
    }
    let select_sql = select_exhausted_production_sql();
    let Some(row) = sqlx::query(sqlx::AssertSqlSafe(select_sql))
        .bind(grace_ms)
        .bind(package_ids)
        .bind(environment)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("select exhausted run", &error))?
    else {
        return Ok(ProductionReapResult::Empty);
    };
    let status_text: String = row_value(&row, 2, "exhausted run status")?;
    let status = RunStatus::from_sql(&status_text).ok_or_else(|| {
        ProductionLeaseError::new(
            ProductionLeaseErrorType::Contract,
            "decode exhausted run",
            format!("unknown run status {status_text:?}"),
        )
    })?;
    let wiring_id: Option<String> = row_value(&row, 3, "exhausted wiring id")?;
    let wiring_version: Option<i32> = row_value(&row, 4, "exhausted wiring version")?;
    let identity = match (wiring_id, wiring_version) {
        (Some(wiring_id), Some(wiring_version)) if !wiring_id.is_empty() && wiring_version > 0 => {
            ExhaustedExecutionIdentity {
                wiring_id,
                wiring_version,
            }
        }
        _ => {
            return Err(ProductionLeaseError::new(
                ProductionLeaseErrorType::Contract,
                "decode exhausted run",
                "run-execution-grain-corrupt",
            ));
        }
    };
    let selected = ExhaustedLease {
        run_id: row_value(&row, 1, "exhausted run id")?,
        status,
        identity,
    };
    if !matches!(selected.status, RunStatus::Dispatched | RunStatus::Running) {
        return Err(ProductionLeaseError::new(
            ProductionLeaseErrorType::Contract,
            "validate exhausted run",
            format!("non-runnable status {}", selected.status.as_sql()),
        ));
    }

    let body = generic_failure_outcome(
        "infrastructure-failure",
        &selected.identity,
        &selected.run_id,
    );
    let terminalize_sql = terminalize_exhausted_production_sql();
    // D2b: this host owns the instant the reap stamps.
    let reaped_at = SystemTime::now();
    let row = sqlx::query(sqlx::AssertSqlSafe(terminalize_sql))
        .bind(&selected.run_id)
        .bind(&body)
        .bind(chrono::DateTime::<chrono::Utc>::from(reaped_at))
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("terminalize exhausted run", &error))?
        .ok_or_else(|| {
            ProductionLeaseError::new(
                ProductionLeaseErrorType::Contract,
                "terminalize exhausted run",
                "selected run or queue row disappeared while locked",
            )
        })?;
    let status: String = row_value(&row, 0, "exhausted terminal status")?;
    if status != RunStatus::InfrastructureFailure.as_sql() {
        return Err(ProductionLeaseError::new(
            ProductionLeaseErrorType::Contract,
            "terminalize exhausted run",
            format!("unexpected status {status:?}"),
        ));
    }
    Ok(ProductionReapResult::Reaped {
        run_id: selected.run_id,
    })
}

fn generic_failure_outcome(
    code: &str,
    identity: &ExhaustedExecutionIdentity,
    run_id: &str,
) -> String {
    let coordinate = serde_json::json!({
        "code": code,
        "wiring-id": identity.wiring_id,
        "wiring-version": identity.wiring_version,
        "run-id": run_id,
    });
    let body = serde_json::json!({ "error": coordinate });
    body.to_string()
}

fn decode_selected_lease(row: &Row) -> Result<SelectedLease, ProductionLeaseError> {
    let status: String = row_value(row, 2, "run status")?;
    let status = RunStatus::from_sql(&status).ok_or_else(|| {
        ProductionLeaseError::new(
            ProductionLeaseErrorType::Contract,
            "decode production lease",
            "unknown run status",
        )
    })?;
    let (wiring_id, wiring_version) = decode_wiring_identity(
        row_value(row, 4, "wiring id")?,
        row_value(row, 5, "wiring version")?,
    )?;
    let payload: String = row_value(row, 3, "input")?;
    let digests: String = row_value(row, 10, "node digests")?;
    let mut workflow = ProductionWorkflow {
        wiring_hash: row_value(row, 6, "wiring hash")?,
        execution_principal_id: row_value(row, 8, "execution principal")?,
        execution_role: row_value(row, 9, "execution role")?,
        node_digests: serde_json::from_str(&digests).map_err(|error| {
            ProductionLeaseError::with_source(
                ProductionLeaseErrorType::Contract,
                "decode frozen node digests",
                error,
            )
        })?,
        causation: None,
    };
    let root: Option<String> = row_value(row, 11, "event root")?;
    let depth: Option<i32> = row_value(row, 12, "event depth")?;
    if let (Some(root), Some(depth)) = (root, depth) {
        workflow.causation = Some(wamn_event_wire::Causation {
            run: row_value(row, 0, "run id")?,
            root,
            depth: u32::try_from(depth).map_err(|error| {
                ProductionLeaseError::with_source(
                    ProductionLeaseErrorType::Contract,
                    "decode event depth",
                    error,
                )
            })?,
        });
    }
    Ok(SelectedLease {
        run_id: row_value(row, 0, "run id")?,
        package_id: row_value(row, 7, "application")?,
        had_prior_lease: row_value(row, 1, "prior lease evidence")?,
        status,
        payload: serde_json::from_str(&payload).map_err(|error| {
            ProductionLeaseError::with_source(
                ProductionLeaseErrorType::Contract,
                "decode run input",
                error,
            )
        })?,
        wiring_id,
        wiring_version,
        workflow: Box::new(workflow),
    })
}

fn decode_wiring_identity(
    wiring_id: Option<String>,
    wiring_version: Option<i32>,
) -> Result<(String, i32), ProductionLeaseError> {
    let (wiring_id, wiring_version) = match (wiring_id, wiring_version) {
        (Some(wiring_id), Some(wiring_version)) if !wiring_id.is_empty() && wiring_version > 0 => {
            (wiring_id, wiring_version)
        }
        (None, None) => {
            return Err(ProductionLeaseError::new(
                ProductionLeaseErrorType::WiringIdentity,
                "decode production lease",
                "run-wiring-identity-missing",
            ));
        }
        _ => {
            return Err(ProductionLeaseError::new(
                ProductionLeaseErrorType::WiringIdentity,
                "decode production lease",
                "run-wiring-identity-corrupt",
            ));
        }
    };
    Ok((wiring_id, wiring_version))
}

fn row_value<T>(row: &Row, index: usize, field: &'static str) -> Result<T, ProductionLeaseError>
where
    for<'value> T: Decode<'value, sqlx::Postgres> + Type<sqlx::Postgres>,
{
    row.try_get(index).map_err(|error| {
        ProductionLeaseError::new(
            ProductionLeaseErrorType::Contract,
            "decode production lease row",
            format!("{field}: {error}"),
        )
    })
}

/// Record deadline changes under the exact lease fence.
pub async fn record_deadline_adjustments(
    connection: &mut sqlx::PgConnection,
    run_id: &str,
    runner: &str,
    lease_generation: i64,
    adjustments: &serde_json::Value,
) -> Result<bool, ProductionLeaseError> {
    validate_generation(lease_generation)?;
    let row = sqlx::query(sqlx::AssertSqlSafe(
        crate::transitions::record_deadline_adjustments_sql(),
    ))
    .bind(run_id)
    .bind(run_id)
    .bind(runner)
    .bind(lease_generation)
    .bind(adjustments.to_string())
    .fetch_one(connection)
    .await
    .map_err(|error| ProductionLeaseError::postgres("store deadline report", &error))?;
    row_value(&row, 0, "deadline report fence")
}

/// Whether this release still has queued work.
pub async fn pinned_backlog(
    connection: &mut sqlx::PgConnection,
    applications: &[String],
    environment: &str,
    manifest_digest: &str,
) -> Result<bool, ProductionLeaseError> {
    validate_target(applications, environment)?;
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM run_queue q JOIN runs r USING (project_env_key, run_id) \
        WHERE q.project_env_key = current_setting('app.project_env_key', true) \
        AND r.package_id = ANY($1::text[]) AND r.environment = $2 AND r.manifest_digest = $3 \
        AND r.status IN ('{}', '{}'))",
        RunStatus::Dispatched.as_sql(),
        RunStatus::Running.as_sql()
    );
    let row = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(applications)
        .bind(environment)
        .bind(manifest_digest)
        .fetch_one(connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("read pinned backlog", &error))?;
    row_value(&row, 0, "pinned backlog")
}

/// Delete expired terminal history on an authorized retention connection.
pub async fn prune_terminal_runs(
    connection: &mut sqlx::PgConnection,
    project_env_key: &str,
    retention_days: u32,
) -> Result<u64, sqlx::Error> {
    sqlx::query(sqlx::AssertSqlSafe(crate::sql::prune_terminal_runs_sql()))
        .bind(project_env_key)
        .bind(i64::from(retention_days))
        .execute(connection)
        .await
        .map(|result| result.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_and_corrupt_wiring_identity_are_dedicated_stable_refusals() {
        let missing = decode_wiring_identity(None, None).unwrap_err();
        assert_eq!(
            missing.error_type(),
            ProductionLeaseErrorType::WiringIdentity
        );

        for corrupt in [
            decode_wiring_identity(Some("orders".into()), None),
            decode_wiring_identity(None, Some(1)),
            decode_wiring_identity(Some(String::new()), Some(1)),
            decode_wiring_identity(Some("orders".into()), Some(0)),
        ] {
            let error = corrupt.unwrap_err();
            assert_eq!(error.error_type(), ProductionLeaseErrorType::WiringIdentity);
        }
    }
}

fn invalid_parameters(detail: &'static str) -> ProductionLeaseError {
    ProductionLeaseError::new(
        ProductionLeaseErrorType::Contract,
        "validate lease parameters",
        detail,
    )
}
fn validate_generation(generation: i64) -> Result<(), ProductionLeaseError> {
    if generation <= 0 {
        return Err(invalid_parameters("lease generation must be positive"));
    }
    Ok(())
}
fn validate_duration(duration_ms: i64) -> Result<(), ProductionLeaseError> {
    if duration_ms <= 0 {
        return Err(invalid_parameters("lease duration must be positive"));
    }
    Ok(())
}
fn validate_target(applications: &[String], environment: &str) -> Result<(), ProductionLeaseError> {
    if applications.is_empty()
        || environment.is_empty()
        || applications.iter().any(|application| {
            application.is_empty()
                || application.trim() != application
                || application.as_bytes().contains(&0)
        })
    {
        return Err(invalid_parameters(
            "nonempty application names and environment are required",
        ));
    }
    Ok(())
}
