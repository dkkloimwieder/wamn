//! Admit a durable run and grant its first lease in one transaction.

use crate::run_store::{ProductionLeaseError, ProductionLeaseErrorType};
use sqlx::Row as _;

/// The source of a durable workflow execution.
#[derive(Debug, Clone, Copy)]
pub enum RunTrigger {
    Request,
    Schedule,
    DataChange,
    RunNow,
}

impl RunTrigger {
    /// Persisted trigger vocabulary.
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Schedule => "schedule",
            Self::DataChange => "event",
            Self::RunNow => "manual",
        }
    }
}

/// Frozen workflow version, input, and execution identity.
#[derive(Debug)]
pub struct RunAdmission<'a> {
    pub application: &'a wamn_registry::ApplicationName,
    pub environment: &'a wamn_registry::EnvName,
    pub workflow_id: &'a wamn_package::canonical_name::CanonicalName,
    pub workflow_version: i32,
    pub graph_hash: &'a wamn_digest::Digest,
    pub execution_principal_id: &'a wamn_identity_protocol::PrincipalId,
    pub execution_role: &'a str,
    pub trigger: RunTrigger,
    pub invoked_by: &'a str,
    pub invoked_by_user_id: Option<&'a wamn_identity_protocol::PrincipalId>,
    pub idempotency_key: &'a str,
    pub input: &'a serde_json::Value,
    pub source_run: Option<&'a str>,
    pub root_run: Option<&'a str>,
    pub depth: Option<i32>,
}

/// Only the winning insert returns execution authority.
#[derive(Debug, PartialEq, Eq)]
pub enum RunAdmitted {
    Leased { run_id: String, generation: i64 },
    Existing { run_id: String },
    Conflict,
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

/// Insert and lease the run atomically on an authorized transaction.
pub async fn admit_run(
    connection: &mut sqlx::PgConnection,
    run: &RunAdmission<'_>,
    runner: &str,
    lease_ttl_ms: i64,
) -> Result<RunAdmitted, ProductionLeaseError> {
    let input = sqlx::types::Json(run.input);
    let inserted = sqlx::query_as(ADMIT_SQL)
        .bind(run.application)
        .bind(run.environment)
        .bind(run.workflow_id)
        .bind(run.workflow_version)
        .bind(run.graph_hash)
        .bind(run.execution_principal_id)
        .bind(run.execution_role)
        .bind(run.trigger.as_sql())
        .bind(run.invoked_by)
        .bind(run.invoked_by_user_id)
        .bind(run.idempotency_key)
        .bind(input)
        .bind(run.source_run)
        .bind(run.root_run)
        .bind(run.depth)
        .bind(runner)
        .bind(lease_ttl_ms)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("admit and lease workflow run", &error))?;
    if let Some((run_id, generation)) = inserted {
        return Ok(RunAdmitted::Leased { run_id, generation });
    }
    let existing = sqlx::query_scalar(EXISTING_SQL)
        .bind(run.application)
        .bind(run.environment)
        .bind(run.workflow_id)
        .bind(run.workflow_version)
        .bind(run.trigger.as_sql())
        .bind(run.idempotency_key)
        .bind(input)
        .bind(run.execution_principal_id)
        .bind(run.execution_role)
        .bind(run.invoked_by_user_id)
        .fetch_optional(connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("read original workflow run", &error))?;
    Ok(match existing {
        Some(run_id) => RunAdmitted::Existing { run_id },
        None => RunAdmitted::Conflict,
    })
}

/// Find the history actor of the CDC row when the source has a history table.
pub async fn history_actor(
    connection: &mut sqlx::PgConnection,
    envelope: &wamn_event::Envelope,
) -> Result<Option<wamn_identity_protocol::PrincipalId>, ProductionLeaseError> {
    let schemas = sqlx::query("SELECT n.nspname::text FROM pg_namespace n JOIN pg_class c ON c.relnamespace = n.oid WHERE c.relname = 'wamn_entities' AND c.relkind = 'r' ORDER BY 1").fetch_all(&mut *connection).await
        .map_err(|error| ProductionLeaseError::postgres("read entity map schemas", &error))?;
    for row in schemas {
        let schema: String = row.get(0);
        let quoted_schema = postgres_protocol::escape::escape_identifier(&schema);
        let sql = format!(
            "SELECT c.oid, c.relname::text FROM {quoted_schema}.wamn_entities m JOIN pg_class c ON c.oid = m.relation_oid WHERE m.package_id = $1 AND m.entity_id = $2 AND c.relname = m.table_name AND c.relnamespace = $3::text::regnamespace"
        );
        let Some(model) = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(&envelope.package_id)
            .bind(&envelope.entity)
            .bind(&schema)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| {
                ProductionLeaseError::postgres("resolve event history model", &error)
            })?
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
            .map_err(|error| ProductionLeaseError::postgres("locate event history", &error))?
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
        return sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(i64::from(envelope.txid))
            .bind(envelope.op.as_str())
            .bind(&image)
            .bind(oid)
            .fetch_optional(&mut *connection)
            .await
            .map(|row| row.and_then(|row| row.get(0)))
            .map_err(|error| ProductionLeaseError::postgres("read event history actor", &error));
    }
    Ok(None)
}
