//! Run persistence over a connection authorized by the caller.

use crate::queue::{
    advance_lease_attempts_sql, grant_production_lease_sql, renew_production_lease_sql,
    select_exhausted_production_sql, select_production_lease_sql,
    terminalize_exhausted_production_sql,
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
        input_json: String,
        checkpoint_json: Option<String>,
        lease_generation: i64,
        wiring_id: wamn_catalog::canonical_name::CanonicalName,
        wiring_version: i32,
        workflow: Box<ProductionWorkflow>,
    },
}

/// The workflow and execution identity fixed at admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductionWorkflow {
    pub wiring_hash: wamn_digest::Digest,
    pub execution_principal_id: wamn_identity_protocol::PrincipalId,
    pub execution_role: String,
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
    status: RunStatus,
    input_json: String,
    checkpoint_json: Option<String>,
    wiring_id: wamn_catalog::canonical_name::CanonicalName,
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
    wiring_id: wamn_catalog::canonical_name::CanonicalName,
    wiring_version: i32,
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
    application: &wamn_catalog::ApplicationName,
    environment: &wamn_registry::EnvName,
    lease_ttl_ms: i64,
) -> Result<LeaseTurn, ProductionLeaseError> {
    validate_duration(lease_ttl_ms)?;
    let select_sql = select_production_lease_sql();
    let Some(row) = sqlx::query(select_sql)
        .bind(application)
        .bind(environment)
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

    // Crash evidence advances in its own statement outside the grant's
    // subtransaction.
    let advance_sql = advance_lease_attempts_sql();
    sqlx::query(advance_sql)
        .bind(&selected.run_id)
        .fetch_one(&mut *connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("advance crash evidence", &error))?;

    let grant_sql = grant_production_lease_sql();
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
    let granted = sqlx::query(grant_sql)
        .bind(&selected.run_id)
        .bind(runner)
        .bind(lease_ttl_ms)
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
            "selected lease disappeared while locked",
        )
    })?;
    let lease_generation = row_value(&row, 0, "lease generation")?;
    Ok(LeaseTurn::Leased(ProductionLeaseResult::Ready {
        run_id: selected.run_id,
        input_json: selected.input_json,
        checkpoint_json: selected.checkpoint_json,
        lease_generation,
        wiring_id: selected.wiring_id,
        wiring_version: selected.wiring_version,
        workflow: selected.workflow,
    }))
}

/// Fail and dequeue one expired run whose retry budget is spent.
pub async fn reap_exhausted(
    connection: &mut sqlx::PgConnection,
    application: &wamn_catalog::ApplicationName,
    environment: &wamn_registry::EnvName,
    grace_ms: i64,
) -> Result<ProductionReapResult, ProductionLeaseError> {
    if grace_ms < 0 {
        return Err(invalid_parameters("janitor grace must be nonnegative"));
    }
    let select_sql = select_exhausted_production_sql();
    let Some(row) = sqlx::query(sqlx::AssertSqlSafe(select_sql))
        .bind(grace_ms)
        .bind(application)
        .bind(environment)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("select exhausted run", &error))?
    else {
        return Ok(ProductionReapResult::Empty);
    };
    let status_text: String = row_value(&row, 1, "exhausted run status")?;
    let status = RunStatus::from_sql(&status_text).ok_or_else(|| {
        ProductionLeaseError::new(
            ProductionLeaseErrorType::Contract,
            "decode exhausted run",
            format!("unknown run status {status_text:?}"),
        )
    })?;
    let wiring_id: Option<wamn_catalog::canonical_name::CanonicalName> =
        row_value(&row, 2, "exhausted wiring id")?;
    let wiring_version: Option<i32> = row_value(&row, 3, "exhausted wiring version")?;
    let identity = match (wiring_id, wiring_version) {
        (Some(wiring_id), Some(wiring_version)) if wiring_version > 0 => {
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
        run_id: row_value(&row, 0, "exhausted run id")?,
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
    let status: String = row_value(row, 1, "run status")?;
    let status = RunStatus::from_sql(&status).ok_or_else(|| {
        ProductionLeaseError::contract("decode production lease", "unknown run status")
    })?;
    let (wiring_id, wiring_version) = decode_wiring_identity(
        row_value(row, 3, "wiring id")?,
        row_value(row, 4, "wiring version")?,
    )?;
    let run_id: String = row_value(row, 0, "run id")?;
    let mut workflow = ProductionWorkflow {
        wiring_hash: row_value(row, 5, "wiring hash")?,
        execution_principal_id: row_value(row, 6, "execution principal")?,
        execution_role: row_value(row, 7, "execution role")?,
        causation: None,
    };
    let root: Option<String> = row_value(row, 9, "event root")?;
    let depth: Option<i32> = row_value(row, 10, "event depth")?;
    if let (Some(root), Some(depth)) = (root, depth) {
        workflow.causation = Some(wamn_event_wire::Causation {
            run: run_id.clone(),
            root,
            depth: u32::try_from(depth)
                .map_err(|error| ProductionLeaseError::invalid("decode event depth", error))?,
        });
    }
    Ok(SelectedLease {
        run_id,
        status,
        input_json: row_value(row, 2, "input")?,
        checkpoint_json: row_value(row, 8, "response checkpoint")?,
        wiring_id,
        wiring_version,
        workflow: Box::new(workflow),
    })
}

fn decode_wiring_identity(
    wiring_id: Option<wamn_catalog::canonical_name::CanonicalName>,
    wiring_version: Option<i32>,
) -> Result<(wamn_catalog::canonical_name::CanonicalName, i32), ProductionLeaseError> {
    let (wiring_id, wiring_version) = match (wiring_id, wiring_version) {
        (Some(wiring_id), Some(wiring_version)) if wiring_version > 0 => {
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

/// Record the Response checkpoint under the live lease fence.
pub async fn checkpoint(
    connection: &mut sqlx::PgConnection,
    run_id: &str,
    runner: &str,
    lease_generation: i64,
    state: &serde_json::Value,
) -> Result<bool, ProductionLeaseError> {
    validate_generation(lease_generation)?;
    let row = sqlx::query(sqlx::AssertSqlSafe(crate::transitions::checkpoint_sql()))
        .bind(run_id)
        .bind(run_id)
        .bind(runner)
        .bind(lease_generation)
        .bind(state.to_string())
        .fetch_one(connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("store Response checkpoint", &error))?;
    row_value(&row, 0, "Response checkpoint fence")
}

/// Check that the host still owns the unexpired lease before dispatching a node.
pub async fn lease_remaining_ms(
    connection: &mut sqlx::PgConnection,
    run: &str,
    runner: &str,
    generation: i64,
) -> Result<Option<i64>, ProductionLeaseError> {
    sqlx::query_scalar(
        "SELECT floor(extract(epoch FROM (lease_expires_at - clock_timestamp())) * 1000)::bigint
           FROM run_queue WHERE run_id = $1 AND lease_owner = $2 AND lease_generation = $3
            AND lease_expires_at > clock_timestamp()",
    )
    .bind(run)
    .bind(runner)
    .bind(generation)
    .fetch_optional(connection)
    .await
    .map_err(|error| ProductionLeaseError::postgres("check workflow node lease", &error))
}

/// Delete expired terminal history on an authorized retention connection.
pub async fn prune_terminal_runs(
    connection: &mut sqlx::PgConnection,
    retention_days: u32,
) -> Result<u64, sqlx::Error> {
    sqlx::query(sqlx::AssertSqlSafe(crate::sql::prune_terminal_runs_sql()))
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
            decode_wiring_identity(Some("orders".parse().unwrap()), None),
            decode_wiring_identity(None, Some(1)),
            decode_wiring_identity(Some("orders".parse().unwrap()), Some(0)),
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
