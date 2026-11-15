//! Atomic run and queue insertion inside the caller's authorized transaction.

use crate::run_store::{ProductionLeaseError, ProductionLeaseErrorType};
use sqlx::Row as _;

/// The supported sources of queued workflow runs.
#[derive(Debug, Clone, Copy)]
pub enum RunTrigger {
    Manual,
    Schedule,
    Event,
}
impl RunTrigger {
    /// Persisted trigger vocabulary.
    pub const fn as_sql(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Schedule => "schedule",
            Self::Event => "event",
        }
    }
}

/// Frozen admission facts. A repeat retains the first run's release and nodes.
#[derive(Debug)]
pub struct RunAdmission<'a> {
    pub project_env_key: &'a str,
    pub application: &'a str,
    pub manifest_digest: &'a str,
    pub environment: &'a str,
    pub workflow_id: &'a str,
    pub workflow_version: i32,
    pub wiring_hash: &'a str,
    pub execution_principal_id: &'a str,
    pub execution_role: &'a str,
    pub trigger: RunTrigger,
    pub invoked_by: &'a str,
    pub invoked_by_user_id: Option<&'a str>,
    pub node_digests: &'a serde_json::Value,
    pub idempotency_key: &'a str,
    pub input: &'a serde_json::Value,
    pub source_run: Option<&'a str>,
    pub root_run: Option<&'a str>,
    pub depth: Option<i32>,
}

/// The result of an idempotent admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunAdmitted {
    Queued { run_id: String },
    Conflict,
}

/// Read a repeat before resolving a new run against the current release.
pub async fn existing_run(
    connection: &mut sqlx::PgConnection,
    project_env_key: &str,
    key: &str,
    input: &serde_json::Value,
    trigger: RunTrigger,
) -> Result<Option<String>, ProductionLeaseError> {
    sqlx::query(crate::queue::select_run_sql())
        .bind(project_env_key)
        .bind(key)
        .bind(input.to_string())
        .bind(trigger.as_sql())
        .fetch_optional(connection)
        .await
        .map(|row| row.map(|row| row.get(0)))
        .map_err(|error| ProductionLeaseError::postgres("read original run", &error))
}

/// Insert the run and its queue row atomically. The caller commits or rolls back both.
pub async fn admit_run(
    connection: &mut sqlx::PgConnection,
    run: &RunAdmission<'_>,
) -> Result<RunAdmitted, ProductionLeaseError> {
    let input = run.input.to_string();
    let inserted = sqlx::query(sqlx::AssertSqlSafe(crate::queue::insert_run_sql()))
        .bind(run.project_env_key)
        .bind(run.application)
        .bind(run.manifest_digest)
        .bind(run.environment)
        .bind(run.workflow_id)
        .bind(run.workflow_version)
        .bind(run.wiring_hash)
        .bind(run.execution_principal_id)
        .bind(run.execution_role)
        .bind(run.trigger.as_sql())
        .bind(run.invoked_by)
        .bind(run.invoked_by_user_id)
        .bind(run.node_digests.to_string())
        .bind(run.idempotency_key)
        .bind(input)
        .bind(run.source_run)
        .bind(run.root_run)
        .bind(run.depth)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| ProductionLeaseError::postgres("admit workflow run", &error))?;
    if let Some(row) = inserted {
        let run_id: String = row.get(0);
        sqlx::query(crate::queue::insert_run_queue_sql())
            .bind(run.project_env_key)
            .bind(&run_id)
            .execute(&mut *connection)
            .await
            .map_err(|error| ProductionLeaseError::postgres("queue workflow run", &error))?;
        return Ok(RunAdmitted::Queued { run_id });
    }
    Ok(
        match existing_run(
            connection,
            run.project_env_key,
            run.idempotency_key,
            run.input,
            run.trigger,
        )
        .await?
        {
            Some(run_id) => RunAdmitted::Queued { run_id },
            None => RunAdmitted::Conflict,
        },
    )
}

/// The enabled version of one manual workflow. A schedule workflow is not
/// started through `workflow.start`. Binds: `$1` `project_env_key`, `$2` environment,
/// `$3` application, `$4` workflow id.
const ENABLED_WORKFLOW_SQL: &str = "\
SELECT version, graph_json::text, wiring_hash, execution_type, execution_role \
  FROM catalog.workflows \
 WHERE project_env_key = $1 AND environment = $2 AND application = $3 \
   AND workflow_id = $4 AND enabled AND trigger = 'manual'";

/// One workflow version, enabled or not: a run finishes under the version it
/// pinned. Binds: `$1` `project_env_key`, `$2` environment, `$3` application, `$4`
/// workflow id, `$5` version.
const WORKFLOW_VERSION_SQL: &str = "\
SELECT graph_json::text, wiring_hash \
  FROM catalog.workflows \
 WHERE project_env_key = $1 AND environment = $2 AND application = $3 \
   AND workflow_id = $4 AND version = $5";

/// Read an enabled manual workflow for admission.
pub async fn enabled_workflow(
    connection: &mut sqlx::PgConnection,
    project_env_key: &str,
    environment: &str,
    application: &str,
    workflow: &str,
) -> Result<Option<sqlx::postgres::PgRow>, sqlx::Error> {
    sqlx::query(ENABLED_WORKFLOW_SQL)
        .bind(project_env_key)
        .bind(environment)
        .bind(application)
        .bind(workflow)
        .fetch_optional(connection)
        .await
}

/// Read the exact workflow version frozen by an admitted run.
pub async fn workflow_version(
    connection: &mut sqlx::PgConnection,
    project_env_key: &str,
    environment: &str,
    application: &str,
    workflow: &str,
    version: i32,
) -> Result<Option<(String, String)>, sqlx::Error> {
    sqlx::query(WORKFLOW_VERSION_SQL)
        .bind(project_env_key)
        .bind(environment)
        .bind(application)
        .bind(workflow)
        .bind(version)
        .fetch_optional(connection)
        .await
        .map(|row| row.map(|row| (row.get(0), row.get(1))))
}

const ENABLED_EVENTS_SQL: &str = "SELECT application, workflow_id, source_package, entity, \
    array_to_json(ops)::text, condition, enable_sequence, version, graph_json::text, \
    wiring_hash, execution_role FROM catalog.workflows \
    WHERE project_env_key = $1 AND environment = $2 AND ($3::text[] IS NULL OR application = ANY($3)) \
      AND enabled AND trigger = 'event' ORDER BY application, workflow_id";

/// Read enabled event workflows. A null application set reads the environment.
pub async fn event_workflows(
    connection: &mut sqlx::PgConnection,
    project_env_key: &str,
    environment: &str,
    applications: Option<&[String]>,
) -> Result<Vec<sqlx::postgres::PgRow>, sqlx::Error> {
    sqlx::query(ENABLED_EVENTS_SQL)
        .bind(project_env_key)
        .bind(environment)
        .bind(applications)
        .fetch_all(connection)
        .await
}

pub async fn history_actor(
    connection: &mut sqlx::PgConnection,
    envelope: &wamn_event_wire::Envelope,
) -> Result<Option<String>, ProductionLeaseError> {
    let schemas = sqlx::query("SELECT n.nspname::text FROM pg_namespace n JOIN pg_class c ON c.relnamespace = n.oid WHERE c.relname = 'wamn_entities' AND c.relkind = 'r' ORDER BY 1").fetch_all(&mut *connection).await
        .map_err(|error| ProductionLeaseError::postgres("read entity map schemas", &error))?;
    for row in schemas {
        let schema: String = row.get(0);
        let quoted_schema = postgres_protocol::escape::escape_identifier(&schema);
        let sql = format!(
            "SELECT c.oid, c.relname::text FROM {quoted_schema}.wamn_entities m JOIN pg_class c ON c.oid = m.relation_oid WHERE m.package_id = $1 AND m.entity_id = $2 AND c.relname = m.table_name AND c.relnamespace = $3::text::regnamespace"
        );
        let Some(model) = sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
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
            wamn_event_wire::Op::Delete => &envelope.old,
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
        let image = serde_json::Value::Object(image.clone()).to_string();
        // History stores a full xid8. CDC carries its low 32 bits. Build the
        // primary key from the same columns and timestamp spelling as history.
        let table = format!(
            "{quoted_schema}.{}",
            postgres_protocol::escape::escape_identifier(&table)
        );
        let sql = format!(
            "WITH image AS (SELECT to_jsonb(jsonb_populate_record(NULL::{table}, $3::text::jsonb)) AS value) SELECT h.changed_by::text FROM {history} h WHERE h.transaction_id % 4294967296 = $1 AND h.type = $2 AND h.row_key = (SELECT jsonb_object_agg(a.attname, CASE WHEN a.atttypid = 'timestamptz'::regtype AND isfinite((image.value ->> a.attname)::timestamptz) THEN to_jsonb(to_char((image.value ->> a.attname)::timestamptz AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')) ELSE image.value -> a.attname END) FROM image, pg_index i JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY(i.indkey) WHERE i.indrelid = $4 AND i.indisprimary) ORDER BY h.position DESC LIMIT 1"
        );
        return sqlx::query(sqlx::AssertSqlSafe(sql.clone()))
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

/// Read the enabled version used for a due schedule admission.
pub async fn enabled_schedule(
    connection: &mut sqlx::PgConnection,
    project_env_key: &str,
    environment: &str,
    application: &str,
    workflow: &str,
) -> Result<Option<sqlx::postgres::PgRow>, sqlx::Error> {
    sqlx::query(
        "SELECT version, graph_json::text, wiring_hash, execution_role FROM catalog.workflows \
        WHERE project_env_key = $1 AND environment = $2 AND application = $3 AND workflow_id = $4 \
        AND enabled AND trigger = 'schedule'",
    )
    .bind(project_env_key)
    .bind(environment)
    .bind(application)
    .bind(workflow)
    .fetch_optional(connection)
    .await
}

/// Read schedules and their calendar minute in this environment's project time zone.
pub async fn enabled_schedules(
    connection: &mut sqlx::PgConnection,
    project_env_key: &str,
    environment: &str,
    applications: &[String],
    project: &str,
    tick: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<(String, String, String, chrono::NaiveDateTime)>, sqlx::Error> {
    sqlx::query("WITH timezone AS (SELECT COALESCE((SELECT value FROM platform.setting \
            WHERE target_type='project' AND target_id=$4 AND key='timezone'), '\"UTC\"'::jsonb) AS value) \
        SELECT application, workflow_id, schedule, \
            $5::timestamptz AT TIME ZONE CASE WHEN jsonb_typeof(timezone.value)='string' \
                THEN timezone.value #>> '{}' ELSE '' END \
        FROM catalog.workflows CROSS JOIN timezone \
        WHERE project_env_key=$1 AND environment=$2 AND application=ANY($3) \
            AND enabled AND trigger='schedule'")
        .bind(project_env_key).bind(environment).bind(applications).bind(project).bind(tick)
        .fetch_all(connection).await.map(|rows| rows.into_iter()
            .map(|row| (row.get(0), row.get(1), row.get(2), row.get(3))).collect())
}
