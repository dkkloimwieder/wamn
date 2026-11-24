//! PostgreSQL storage for workflow definitions and version changes.

pub mod admission;

use serde_json::Value;
use sqlx::{PgConnection, Row as _, postgres::PgRow};
use wamn_application::application_config::ApplicationConfig;
use wamn_digest::Digest;
use wamn_identity_protocol::PrincipalId;
use wamn_package::canonical_name::CanonicalName;
use wamn_workflow_graph::{GraphTrigger, WorkflowGraph};

/// Take the transaction lock used when authoring a new version.
pub async fn try_author_lock(
    connection: &mut PgConnection,
    app: &ApplicationConfig,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(super::ENVIRONMENT_TRANSACTION_LOCK_SQL)
        .bind(app.org.as_str())
        .bind(app.environment.as_str())
        .fetch_one(connection)
        .await
}

/// Commit the version change while retaining the session lock.
pub async fn commit_change(connection: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query("COMMIT").execute(connection).await?;
    Ok(())
}

/// Roll back the version change while retaining the session lock.
pub async fn rollback_change(connection: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::raw_sql("ROLLBACK").execute(connection).await?;
    Ok(())
}

/// Store a validated graph and derive the index columns from its entry.
/// Returns the graph hash if a new version was stored.
pub async fn insert_version(
    connection: &mut PgConnection,
    app: &ApplicationConfig,
    graph: &WorkflowGraph,
    version: i32,
    entry: &GraphTrigger,
    role: Option<&str>,
    principal: &PrincipalId,
) -> Result<Option<Digest>, sqlx::Error> {
    let mut path = None;
    let mut schedule = None;
    let mut source_package = None;
    let mut source_entity = None;
    let mut source_ops = None;
    let mut source_condition = None;
    let trigger = match entry {
        GraphTrigger::Request { path: request_path } => {
            path = Some(request_path);
            "request"
        }
        GraphTrigger::Schedule { cron } => {
            schedule = Some(cron);
            "schedule"
        }
        GraphTrigger::DataChange {
            package,
            entity,
            ops,
            condition,
        } => {
            source_package = Some(package.as_str());
            source_entity = Some(entity.as_str());
            source_ops = Some(ops.iter().map(|op| op.as_str()).collect::<Vec<_>>());
            source_condition = condition.as_deref();
            "data_change"
        }
    };
    let hash = graph.graph_hash();
    let inserted = sqlx::query(
        "INSERT INTO catalog.workflows
          (environment, application, workflow_id, version, graph_json, graph_hash,
           trigger, path, schedule, source_package, entity, ops, condition,
           execution_type, execution_role, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)
         ON CONFLICT (environment, application, workflow_id, version) DO NOTHING",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .bind(graph.graph_id.as_str())
    .bind(version)
    .bind(sqlx::types::Json(graph))
    .bind(hash.to_string())
    .bind(trigger)
    .bind(path)
    .bind(schedule)
    .bind(source_package)
    .bind(source_entity)
    .bind(source_ops)
    .bind(source_condition)
    .bind(if role.is_some() { "role" } else { "invoker" })
    .bind(role)
    .bind(uuid::Uuid::from(*principal))
    .execute(connection)
    .await?
    .rows_affected();
    Ok((inserted != 0).then_some(hash))
}

/// Check whether another enabled workflow uses the same request path.
pub async fn path_taken(
    connection: &mut PgConnection,
    app: &ApplicationConfig,
    id: &CanonicalName,
    path: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM catalog.workflows
           WHERE environment = $1 AND application = $2 AND enabled
             AND path = $3 AND workflow_id <> $4)",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .bind(path)
    .bind(id.as_str())
    .fetch_one(connection)
    .await
}

/// A stored version, decoded in the order used when enabling it.
#[derive(Debug)]
pub struct WorkflowVersion(PgRow);

impl WorkflowVersion {
    /// Read the stored graph.
    pub fn graph(&self) -> Result<Value, sqlx::Error> {
        self.0.try_get("graph_json")
    }

    /// Read the stored graph identity.
    pub fn hash(&self) -> Result<Digest, sqlx::Error> {
        graph_hash(&self.0)
    }

    /// Read the configured execution role.
    pub fn role(&self) -> Result<Option<&str>, sqlx::Error> {
        self.0.try_get("execution_role")
    }

    /// Whether this version is enabled.
    pub fn enabled(&self) -> Result<bool, sqlx::Error> {
        self.0.try_get("enabled")
    }
}

fn graph_hash(row: &PgRow) -> Result<Digest, sqlx::Error> {
    row.try_get::<&str, _>("graph_hash")?
        .parse()
        .map_err(|source| sqlx::Error::ColumnDecode {
            index: "graph_hash".into(),
            source: Box::new(source),
        })
}

/// Load the exact version selected for a lifecycle change.
pub async fn load_version(
    connection: &mut PgConnection,
    app: &ApplicationConfig,
    id: &CanonicalName,
    version: i32,
) -> Result<Option<WorkflowVersion>, sqlx::Error> {
    sqlx::query(
        "SELECT graph_json, graph_hash, execution_role, enabled FROM catalog.workflows
          WHERE environment = $1 AND application = $2 AND workflow_id = $3 AND version = $4",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .bind(id.as_str())
    .bind(version)
    .fetch_optional(connection)
    .await
    .map(|row| row.map(WorkflowVersion))
}

/// Whether a prior data-change version could still have a broker consumer.
pub async fn previously_had_consumer(
    connection: &mut PgConnection,
    app: &ApplicationConfig,
    id: &CanonicalName,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM catalog.workflows
                  WHERE environment = $1 AND application = $2 AND workflow_id = $3
                    AND trigger = 'data_change' AND enable_lsn IS NOT NULL)",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .bind(id.as_str())
    .fetch_one(connection)
    .await
}

/// Enable a version at a fresh WAL boundary and report whether it replaced a data-change version.
pub async fn enable_version(
    connection: &mut PgConnection,
    app: &ApplicationConfig,
    id: &CanonicalName,
    version: i32,
) -> Result<bool, sqlx::Error> {
    let lsn: String = sqlx::query_scalar("SELECT pg_current_wal_lsn()::text")
        .fetch_one(&mut *connection)
        .await?;
    let previous: Option<String> = sqlx::query_scalar(
        "UPDATE catalog.workflows SET enabled = false
          WHERE environment = $1 AND application = $2 AND workflow_id = $3 AND enabled
          RETURNING trigger",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .bind(id.as_str())
    .fetch_optional(&mut *connection)
    .await?;
    sqlx::query(
        "UPDATE catalog.workflows SET enabled = true, enable_lsn = $5::pg_lsn
          WHERE environment = $1 AND application = $2 AND workflow_id = $3 AND version = $4",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .bind(id.as_str())
    .bind(version)
    .bind(lsn)
    .execute(connection)
    .await?;
    Ok(previous.as_deref() == Some("data_change"))
}

/// Disable the selected version and return its trigger, if the version exists.
pub async fn disable_version(
    connection: &mut PgConnection,
    app: &ApplicationConfig,
    id: &CanonicalName,
    version: i32,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE catalog.workflows SET enabled = false
          WHERE environment = $1 AND application = $2 AND workflow_id = $3 AND version = $4
          RETURNING trigger",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .bind(id.as_str())
    .bind(version)
    .fetch_optional(connection)
    .await
}

/// Whether an enabled data-change version still owns the consumer.
pub async fn has_enabled_data_change(
    connection: &mut PgConnection,
    app: &ApplicationConfig,
    id: &CanonicalName,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM catalog.workflows
          WHERE environment = $1 AND application = $2 AND workflow_id = $3
            AND enabled AND trigger = 'data_change')",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .bind(id.as_str())
    .fetch_one(connection)
    .await
}

/// Read the workflow rows displayed by the host's administration route.
pub async fn list(
    connection: &mut PgConnection,
    app: &ApplicationConfig,
) -> Result<Value, sqlx::Error> {
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(w) FROM catalog.workflows w WHERE environment = $1 AND application = $2
          ORDER BY workflow_id, version",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .fetch_all(connection)
    .await?;
    Ok(Value::Array(rows))
}

/// Read ids for the enabled workflows that can receive a Request.
pub async fn request_workflow_ids(
    connection: &mut PgConnection,
    app: &ApplicationConfig,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT workflow_id FROM catalog.workflows
          WHERE environment = $1 AND application = $2 AND enabled AND trigger = 'request'",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .fetch_all(connection)
    .await
}
