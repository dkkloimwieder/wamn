//! PostgreSQL storage for workflow runs.

mod pruning;
mod queue;
mod transitions;

use self::queue::{
    advance_lease_attempts_sql, grant_production_lease_sql, renew_production_lease_sql,
    select_exhausted_production_sql, select_production_lease_sql,
    terminalize_exhausted_production_sql,
};
use self::transitions::terminalize_sql;
use sqlx::postgres::PgRow as Row;
use sqlx::{Decode, Row as _, Type};
use std::time::SystemTime;
use wamn_package::canonical_name::CanonicalName;
use wamn_run_state::admission::{RunAdmission, RunAdmitted};
use wamn_run_state::persistence::{LeaseTurn, ProductionLeaseResult, ProductionWorkflow};
use wamn_run_state::run_store::{
    ProductionCompletion, ProductionCompletionResult, ProductionLeaseError,
    ProductionLeaseErrorType, ProductionLeaseRenewal, ProductionReapResult, RunStore,
};
use wamn_run_state::transitions::TerminalizeResult;
use wamn_run_state::{FailType, RunStatus};

/// Run storage on a PostgreSQL connection the caller has already authorized.
#[derive(Debug)]
pub struct PostgresRunStore<'connection> {
    connection: &'connection mut sqlx::PgConnection,
}

impl<'connection> PostgresRunStore<'connection> {
    /// Borrow the connection for run storage.
    pub fn new(connection: &'connection mut sqlx::PgConnection) -> Self {
        Self { connection }
    }
}

const ADMIT_SQL: &str = "
    WITH admitted AS (
        INSERT INTO runs (package_id, environment, graph_id, graph_version, graph_hash,
          execution_principal_id, execution_role, trigger_source, invoked_by, invoked_by_user_id,
          idempotency_key, input_json, status, event_source_run_id, event_root_run_id, event_depth)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, 'running', $13, $14, $15)
        ON CONFLICT (environment, package_id, graph_id, graph_version, trigger_source, idempotency_key)
          WHERE idempotency_key IS NOT NULL DO NOTHING
        RETURNING run_id
    )
    INSERT INTO run_queue (run_id, lease_owner, lease_expires_at, lease_generation)
    SELECT run_id, $16, clock_timestamp() + ($17::bigint * interval '1 millisecond'), 1
      FROM admitted
    RETURNING run_id, lease_generation";

const EXISTING_SQL: &str = "
    SELECT run_id FROM runs
     WHERE package_id = $1 AND environment = $2 AND graph_id = $3 AND graph_version = $4
       AND trigger_source = $5 AND idempotency_key = $6 AND input_json = $7
       AND execution_principal_id = $8 AND execution_role = $9
       AND invoked_by_user_id IS NOT DISTINCT FROM $10";

impl RunStore for PostgresRunStore<'_> {
    async fn admit_run(
        &mut self,
        run: &RunAdmission<'_>,
        runner: &str,
        lease_ttl_ms: i64,
    ) -> Result<RunAdmitted, ProductionLeaseError> {
        let connection = &mut *self.connection;
        let input = sqlx::types::Json(run.input);
        let inserted = sqlx::query_as(ADMIT_SQL)
            .bind(run.application.as_str())
            .bind(run.environment.as_str())
            .bind(run.workflow_id.as_str())
            .bind(run.workflow_version)
            .bind(run.graph_hash.to_string())
            .bind(sqlx::types::Uuid::from(*run.execution_principal_id))
            .bind(run.execution_role)
            .bind(run.trigger.as_sql())
            .bind(run.invoked_by)
            .bind(run.invoked_by_user_id.copied().map(sqlx::types::Uuid::from))
            .bind(run.idempotency_key)
            .bind(input)
            .bind(run.source_run)
            .bind(run.root_run)
            .bind(run.depth)
            .bind(runner)
            .bind(lease_ttl_ms)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| postgres_error("admit and lease workflow run", &error))?;
        if let Some((run_id, generation)) = inserted {
            return Ok(RunAdmitted::Leased { run_id, generation });
        }
        let existing = sqlx::query_scalar(EXISTING_SQL)
            .bind(run.application.as_str())
            .bind(run.environment.as_str())
            .bind(run.workflow_id.as_str())
            .bind(run.workflow_version)
            .bind(run.trigger.as_sql())
            .bind(run.idempotency_key)
            .bind(input)
            .bind(sqlx::types::Uuid::from(*run.execution_principal_id))
            .bind(run.execution_role)
            .bind(run.invoked_by_user_id.copied().map(sqlx::types::Uuid::from))
            .fetch_optional(connection)
            .await
            .map_err(|error| postgres_error("read original workflow run", &error))?;
        Ok(match existing {
            Some(run_id) => RunAdmitted::Existing { run_id },
            None => RunAdmitted::Conflict,
        })
    }

    async fn history_actor(
        &mut self,
        envelope: &wamn_event::Envelope,
    ) -> Result<Option<wamn_identity_protocol::PrincipalId>, ProductionLeaseError> {
        let connection = &mut *self.connection;
        let schemas = sqlx::query("SELECT n.nspname::text FROM pg_namespace n JOIN pg_class c ON c.relnamespace = n.oid WHERE c.relname = 'wamn_entities' AND c.relkind = 'r' ORDER BY 1").fetch_all(&mut *connection).await
            .map_err(|error| postgres_error("read entity map schemas", &error))?;
        for row in schemas {
            let schema: String = row.get(0);
            let quoted_schema = postgres_protocol::escape::escape_identifier(&schema);
            let sql = format!(
                "SELECT c.oid, c.relname::text FROM {quoted_schema}.wamn_entities m JOIN pg_class c ON c.oid = m.relation_oid WHERE m.package_id = $1 AND m.entity_id = $2 AND c.relname = m.table_name AND c.relnamespace = $3::text::regnamespace"
            );
            let Some(model) = sqlx::query(sqlx::AssertSqlSafe(sql))
                .bind(envelope.package_id.as_str())
                .bind(&envelope.entity)
                .bind(&schema)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| postgres_error("resolve event history model", &error))?
            else {
                continue;
            };
            let oid: sqlx::postgres::types::Oid = model.get(0);
            let table: String = model.get(1);
            let history = format!(
                "{quoted_schema}.{}",
                postgres_protocol::escape::escape_identifier(&format!("{table}_history"))
            );
            let present: bool = sqlx::query("SELECT to_regclass($1) IS NOT NULL")
                .bind(&history)
                .fetch_one(&mut *connection)
                .await
                .map_err(|error| postgres_error("locate event history", &error))?
                .get(0);
            if !present {
                return Ok(None);
            }
            let image = match envelope.op {
                wamn_package::RowOperation::Delete => &envelope.old,
                _ => &envelope.new,
            }
            .as_ref()
            .ok_or_else(|| {
                ProductionLeaseError::new(
                    ProductionLeaseErrorType::Contract,
                    "read event row key",
                    "event image is absent",
                )
            })?;
            let image = serde_json::to_string(image)
                .map_err(|error| ProductionLeaseError::invalid("encode event row key", error))?;
            // History stores a full xid8. CDC carries its low 32 bits. Build the
            // primary key from the same columns and timestamp spelling as history.
            let table = format!(
                "{quoted_schema}.{}",
                postgres_protocol::escape::escape_identifier(&table)
            );
            let sql = format!(
                "WITH image AS (SELECT to_jsonb(jsonb_populate_record(NULL::{table}, $3::text::jsonb)) AS value) SELECT h.changed_by FROM {history} h WHERE h.transaction_id % 4294967296 = $1 AND h.type = $2 AND h.row_key = (SELECT jsonb_object_agg(a.attname, CASE WHEN a.atttypid = 'timestamptz'::regtype AND isfinite((image.value ->> a.attname)::timestamptz) THEN to_jsonb(to_char((image.value ->> a.attname)::timestamptz AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')) ELSE image.value -> a.attname END) FROM image, pg_index i JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) WHERE i.indrelid = $4 AND i.indisprimary) ORDER BY h.position DESC LIMIT 1"
            );
            return sqlx::query_scalar::<_, Option<sqlx::types::Uuid>>(sqlx::AssertSqlSafe(sql))
                .bind(i64::from(envelope.txid))
                .bind(envelope.op.as_str())
                .bind(&image)
                .bind(oid)
                .fetch_optional(&mut *connection)
                .await
                .map(|row| row.flatten().map(wamn_identity_protocol::PrincipalId::from))
                .map_err(|error| postgres_error("read event history actor", &error));
        }
        Ok(None)
    }

    async fn renew(
        &mut self,
        run_id: &str,
        runner: &str,
        lease_generation: i64,
        lease_ttl_ms: i64,
    ) -> Result<ProductionLeaseRenewal, ProductionLeaseError> {
        let connection = &mut *self.connection;
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
            .map_err(|error| postgres_error("renew production lease", &error))?;
        Ok(if renewed.is_some() {
            ProductionLeaseRenewal::Renewed
        } else {
            ProductionLeaseRenewal::FenceLost
        })
    }

    async fn complete(
        &mut self,
        run_id: &str,
        runner: &str,
        lease_generation: i64,
        completion: &ProductionCompletion,
    ) -> Result<ProductionCompletionResult, ProductionLeaseError> {
        let connection = &mut *self.connection;
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
            .map_err(|error| postgres_error("terminalize production run", &error))?;
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

    async fn lease_next(
        &mut self,
        runner: &str,
        application: &wamn_registry::ApplicationName,
        environment: &wamn_registry::EnvName,
        lease_ttl_ms: i64,
    ) -> Result<LeaseTurn, ProductionLeaseError> {
        let connection = &mut *self.connection;
        validate_duration(lease_ttl_ms)?;
        let select_sql = select_production_lease_sql();
        let Some(row) = sqlx::query(select_sql)
            .bind(application.as_str())
            .bind(environment.as_str())
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| postgres_error("select production lease", &error))?
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
            .map_err(|error| postgres_error("advance crash evidence", &error))?;

        let grant_sql = grant_production_lease_sql();
        // The grant is the one abortable write left in this transaction, so it runs
        // in its own subtransaction: a database refusal rolls back to the savepoint
        // instead of the whole transaction, leaving the advance above committable.
        sqlx::raw_sql("SAVEPOINT wamn_production_grant")
            .execute(&mut *connection)
            .await
            .map(|_| ())
            .map_err(|error| postgres_error("open production lease savepoint", &error))?;
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
                        postgres_error("roll back refused production lease", &rollback)
                    })?;
                return Ok(LeaseTurn::GrantRefused(postgres_error(
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
            graph_id: selected.graph_id,
            graph_version: selected.graph_version,
            workflow: selected.workflow,
        }))
    }

    async fn reap_exhausted(
        &mut self,
        application: &wamn_registry::ApplicationName,
        environment: &wamn_registry::EnvName,
        grace_ms: i64,
    ) -> Result<ProductionReapResult, ProductionLeaseError> {
        let connection = &mut *self.connection;
        if grace_ms < 0 {
            return Err(invalid_parameters("janitor grace must be nonnegative"));
        }
        let select_sql = select_exhausted_production_sql();
        let Some(row) = sqlx::query(sqlx::AssertSqlSafe(select_sql))
            .bind(grace_ms)
            .bind(application.as_str())
            .bind(environment.as_str())
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| postgres_error("select exhausted run", &error))?
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
        let graph_id = graph_id_value(&row, 2, "exhausted graph id")?;
        let graph_version: Option<i32> = row_value(&row, 3, "exhausted graph version")?;
        let identity = match (graph_id, graph_version) {
            (Some(graph_id), Some(graph_version)) if graph_version > 0 => {
                ExhaustedExecutionIdentity {
                    graph_id,
                    graph_version,
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
            .map_err(|error| postgres_error("terminalize exhausted run", &error))?
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

    async fn checkpoint(
        &mut self,
        run_id: &str,
        runner: &str,
        lease_generation: i64,
        state: &serde_json::Value,
    ) -> Result<bool, ProductionLeaseError> {
        let connection = &mut *self.connection;
        validate_generation(lease_generation)?;
        let row = sqlx::query(sqlx::AssertSqlSafe(transitions::checkpoint_sql()))
            .bind(run_id)
            .bind(run_id)
            .bind(runner)
            .bind(lease_generation)
            .bind(state.to_string())
            .fetch_one(connection)
            .await
            .map_err(|error| postgres_error("store Response checkpoint", &error))?;
        row_value(&row, 0, "Response checkpoint fence")
    }

    async fn lease_remaining_ms(
        &mut self,
        run: &str,
        runner: &str,
        generation: i64,
    ) -> Result<Option<i64>, ProductionLeaseError> {
        let connection = &mut *self.connection;
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
        .map_err(|error| postgres_error("check workflow node lease", &error))
    }

    async fn prune_terminal_runs(
        &mut self,
        retention_days: u32,
    ) -> Result<u64, ProductionLeaseError> {
        let connection = &mut *self.connection;
        sqlx::query(sqlx::AssertSqlSafe(pruning::prune_terminal_runs_sql()))
            .bind(i64::from(retention_days))
            .execute(connection)
            .await
            .map(|result| result.rows_affected())
            .map_err(|error| ProductionLeaseError::storage("prune terminal runs", error))
    }
}

#[derive(Debug)]
struct SelectedLease {
    run_id: String,
    status: RunStatus,
    input_json: String,
    checkpoint_json: Option<String>,
    graph_id: CanonicalName,
    graph_version: i32,
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
    graph_id: CanonicalName,
    graph_version: i32,
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

fn generic_failure_outcome(
    code: &str,
    identity: &ExhaustedExecutionIdentity,
    run_id: &str,
) -> String {
    let coordinate = serde_json::json!({
        "code": code,
        "graph-id": identity.graph_id,
        "graph-version": identity.graph_version,
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
    let (graph_id, graph_version) = decode_graph_identity(
        graph_id_value(row, 3, "graph id")?,
        row_value(row, 4, "graph version")?,
    )?;
    let run_id: String = row_value(row, 0, "run id")?;
    let graph_hash = row_value::<&str>(row, 5, "graph hash")?
        .parse()
        .map_err(|error| {
            ProductionLeaseError::invalid("decode production lease graph hash", error)
        })?;
    let principal = row_value::<sqlx::types::Uuid>(row, 6, "execution principal")?;
    let mut workflow = ProductionWorkflow {
        graph_hash,
        execution_principal_id: wamn_identity_protocol::PrincipalId::from(principal),
        execution_role: row_value(row, 7, "execution role")?,
        causation: None,
    };
    let root: Option<String> = row_value(row, 9, "event root")?;
    let depth: Option<i32> = row_value(row, 10, "event depth")?;
    if let (Some(root), Some(depth)) = (root, depth) {
        workflow.causation = Some(wamn_event::Causation {
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
        graph_id,
        graph_version,
        workflow: Box::new(workflow),
    })
}

fn decode_graph_identity(
    graph_id: Option<CanonicalName>,
    graph_version: Option<i32>,
) -> Result<(CanonicalName, i32), ProductionLeaseError> {
    let (graph_id, graph_version) = match (graph_id, graph_version) {
        (Some(graph_id), Some(graph_version)) if graph_version > 0 => (graph_id, graph_version),
        (None, None) => {
            return Err(ProductionLeaseError::new(
                ProductionLeaseErrorType::GraphIdentity,
                "decode production lease",
                "run-graph-identity-missing",
            ));
        }
        _ => {
            return Err(ProductionLeaseError::new(
                ProductionLeaseErrorType::GraphIdentity,
                "decode production lease",
                "run-graph-identity-corrupt",
            ));
        }
    };
    Ok((graph_id, graph_version))
}

fn row_value<'value, T>(
    row: &'value Row,
    index: usize,
    field: &'static str,
) -> Result<T, ProductionLeaseError>
where
    T: Decode<'value, sqlx::Postgres> + Type<sqlx::Postgres>,
{
    row.try_get(index)
        .map_err(|error| decode_row_error(field, &error))
}

fn graph_id_value(
    row: &Row,
    index: usize,
    field: &'static str,
) -> Result<Option<CanonicalName>, ProductionLeaseError> {
    row_value::<Option<String>>(row, index, field)?
        .map(CanonicalName::try_from)
        .transpose()
        .map_err(|source| {
            decode_row_error(
                field,
                &sqlx::Error::ColumnDecode {
                    index: index.to_string(),
                    source: Box::new(source),
                },
            )
        })
}

fn decode_row_error(field: &str, error: &sqlx::Error) -> ProductionLeaseError {
    ProductionLeaseError::new(
        ProductionLeaseErrorType::Contract,
        "decode production lease row",
        format!("{field}: {error}"),
    )
}

/// Preserve the database message and constraint, without row-bearing DETAIL or HINT.
/// Run rows contain application inputs, results, and invocation context.
pub fn postgres_error(operation: &'static str, error: &sqlx::Error) -> ProductionLeaseError {
    let detail = match error.as_database_error() {
        Some(database) => match database.constraint() {
            Some(constraint) => format!("{} ({constraint})", database.message()),
            None => database.message().to_owned(),
        },
        None => error.to_string(),
    };
    ProductionLeaseError::new(ProductionLeaseErrorType::Storage, operation, detail)
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_and_corrupt_graph_identity_are_dedicated_stable_refusals() {
        let missing = decode_graph_identity(None, None).unwrap_err();
        assert_eq!(
            missing.error_type(),
            ProductionLeaseErrorType::GraphIdentity
        );

        for corrupt in [
            decode_graph_identity(Some("orders".parse().unwrap()), None),
            decode_graph_identity(None, Some(1)),
            decode_graph_identity(Some("orders".parse().unwrap()), Some(0)),
        ] {
            let error = corrupt.unwrap_err();
            assert_eq!(error.error_type(), ProductionLeaseErrorType::GraphIdentity);
        }
    }
}
