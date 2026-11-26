//! PostgreSQL lookups for workflow deliveries under the caller's transaction.

use crate::run_store::ProductionLeaseError;
use sqlx::{PgConnection, Row as _, postgres::PgRow};
use wamn_app::app_config::AppConfig;
use wamn_digest::Digest;
use wamn_event::Envelope;
use wamn_package::canonical_name::CanonicalName;

use crate::run_store::postgres_error;

use super::graph_hash;

/// Stored workflow facts for run admission.
#[derive(Debug)]
pub struct Definition {
    pub id: CanonicalName,
    pub version: i32,
    pub graph: String,
    pub hash: Digest,
    pub role: Option<String>,
}

fn workflow_id(row: &PgRow) -> Result<CanonicalName, sqlx::Error> {
    row.try_get::<String, _>("workflow_id")?
        .try_into()
        .map_err(|source| sqlx::Error::ColumnDecode {
            index: "\"workflow_id\"".into(),
            source: Box::new(source),
        })
}

impl Definition {
    fn from_row(row: &PgRow) -> Result<Self, ProductionLeaseError> {
        let error = |error| ProductionLeaseError::storage("read workflow version", error);
        Ok(Self {
            id: workflow_id(row).map_err(error)?,
            version: row.try_get("version").map_err(error)?,
            graph: row.try_get("graph").map_err(error)?,
            hash: graph_hash(row).map_err(error)?,
            role: row.try_get("execution_role").map_err(error)?,
        })
    }
}

/// One locked schedule candidate. The graph comes into use only if the tick matches.
#[derive(Debug)]
pub struct ScheduledWorkflow(PgRow);

impl ScheduledWorkflow {
    /// Read the candidate's cron expression.
    pub fn schedule(&self) -> Result<&str, ProductionLeaseError> {
        self.0
            .try_get("schedule")
            .map_err(|error| ProductionLeaseError::storage("read schedule", error))
    }

    /// Read the graph for a matching tick.
    pub fn definition(&self) -> Result<Definition, ProductionLeaseError> {
        Definition::from_row(&self.0)
    }

    /// Read the execution role required for the schedule.
    pub fn role(&self) -> Result<&str, ProductionLeaseError> {
        self.0
            .try_get("execution_role")
            .map_err(|error| ProductionLeaseError::storage("read schedule role", error))
    }
}

/// Read and lock the enabled schedule candidates in workflow order.
pub async fn schedules(
    connection: &mut PgConnection,
    app: &AppConfig,
) -> Result<Vec<ScheduledWorkflow>, ProductionLeaseError> {
    sqlx::query(
        "SELECT workflow_id, version, graph_json::text AS graph, graph_hash,
                        execution_role, schedule
                   FROM catalog.workflow
                  WHERE env = $1 AND app = $2 AND enabled AND trigger = 'schedule'
                  ORDER BY workflow_id FOR SHARE",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .fetch_all(connection)
    .await
    .map_err(|error| postgres_error("read enabled schedules", &error))
    .map(|rows| rows.into_iter().map(ScheduledWorkflow).collect())
}

/// Read the workflow ids with enabled data-change versions.
pub async fn consumer_workflows(
    connection: &mut PgConnection,
    app: &AppConfig,
) -> Result<Vec<CanonicalName>, sqlx::Error> {
    sqlx::query(
        "SELECT workflow_id FROM catalog.workflow
                  WHERE env = $1 AND app = $2 AND enabled AND trigger = 'data_change'
                  ORDER BY workflow_id",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .try_map(|row| workflow_id(&row))
    .fetch_all(connection)
    .await
}

/// A locked delivery candidate whose source and WAL position match.
#[derive(Debug)]
pub struct DataChangeWorkflow(PgRow);

impl DataChangeWorkflow {
    /// Read the condition to evaluate against the event.
    pub fn condition(&self) -> Result<Option<&str>, ProductionLeaseError> {
        self.0
            .try_get("condition")
            .map_err(|error| ProductionLeaseError::storage("read data-change condition", error))
    }

    /// Read the graph for a matching event.
    pub fn definition(&self) -> Result<Definition, ProductionLeaseError> {
        Definition::from_row(&self.0)
    }

    /// Read the execution role required for a data change.
    pub fn role(&self) -> Result<&str, ProductionLeaseError> {
        self.0
            .try_get("execution_role")
            .map_err(|error| ProductionLeaseError::storage("read data-change role", error))
    }
}

/// Select an enabled data-change version sharing the event's source and WAL range.
pub async fn data_change(
    connection: &mut PgConnection,
    app: &AppConfig,
    id: &str,
    envelope: &Envelope,
) -> Result<Option<DataChangeWorkflow>, ProductionLeaseError> {
    sqlx::query(
        "SELECT workflow_id, version, graph_json::text AS graph, graph_hash, execution_role, condition
                   FROM catalog.workflow
                  WHERE env = $1 AND app = $2 AND workflow_id = $3
                    AND enabled AND trigger = 'data_change' AND source_package = $4
                    AND entity = $5 AND $6 = ANY(ops) AND enable_lsn <= $7::text::pg_lsn
                    FOR SHARE",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .bind(id)
    .bind(envelope.package_id.as_str())
    .bind(&envelope.entity)
    .bind(envelope.op.as_str())
    // The ID was checked against the envelope's complete source coordinate.
    .bind(wal_lsn(envelope.identity.commit_lsn))
    .fetch_optional(connection)
    .await
    .map_err(|error| postgres_error("match data-change workflow", &error))
    .map(|row| row.map(DataChangeWorkflow))
}

/// Read the exact graph recorded on the leased run.
pub async fn leased_graph(
    connection: &mut PgConnection,
    app: &AppConfig,
    graph_id: &CanonicalName,
    graph_version: i32,
    hash: &Digest,
) -> Result<String, ProductionLeaseError> {
    sqlx::query_scalar(
        "SELECT graph_json::text FROM catalog.workflow
                  WHERE env = $1 AND app = $2 AND workflow_id = $3
                    AND version = $4 AND graph_hash = $5",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .bind(graph_id.as_str())
    .bind(graph_version)
    .bind(hash.to_string())
    .fetch_one(connection)
    .await
    .map_err(|error| postgres_error("read the leased workflow version", &error))
}

/// Read and lock the enabled workflow attached to a request path.
pub async fn request_workflow(
    connection: &mut PgConnection,
    app: &AppConfig,
    path: &str,
) -> Result<Option<Definition>, ProductionLeaseError> {
    sqlx::query(
        "SELECT workflow_id, version, graph_json::text AS graph, graph_hash, execution_role
                   FROM catalog.workflow WHERE env = $1 AND app = $2
                    AND enabled AND trigger = 'request' AND path = $3 FOR SHARE",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .bind(path)
    .fetch_optional(connection)
    .await
    .map_err(|error| postgres_error("load Request workflow", &error))?
    .as_ref()
    .map(Definition::from_row)
    .transpose()
}

/// Read and lock the enabled workflow selected by an administrator.
pub async fn run_now_workflow(
    connection: &mut PgConnection,
    app: &AppConfig,
    id: &str,
) -> Result<Option<Definition>, ProductionLeaseError> {
    sqlx::query(
        "SELECT workflow_id, version, graph_json::text AS graph, graph_hash, execution_role
                   FROM catalog.workflow WHERE env = $1 AND app = $2
                    AND enabled AND workflow_id = $3 FOR SHARE",
    )
    .bind(app.environment.as_str())
    .bind(app.application.as_str())
    .bind(id)
    .fetch_optional(connection)
    .await
    .map_err(|error| postgres_error("load run-now workflow", &error))?
    .as_ref()
    .map(Definition::from_row)
    .transpose()
}

fn wal_lsn(lsn: u64) -> String {
    format!("{:X}/{:08X}", lsn >> 32, lsn & u64::from(u32::MAX))
}
