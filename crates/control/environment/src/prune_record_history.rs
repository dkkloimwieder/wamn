//! The `prune-record-history` subcommand: the record history retention task.
//!
//! The verb removes expired log entries from the history tables of one project
//! database. A relation keeps its entries for n whole days when its
//! `wamn_record_history_log` trigger argument is `P<n>D`, and the verb reads that
//! argument from `pg_trigger`. It skips an `unlimited` relation.
//!
//! **Role:** the verb connects as a targeted `wamn_audit_retention` credential
//! generation and refuses any other login. apply-package grants that role
//! `DELETE` and `SELECT (row_key, position, changed_at, transaction_id)` on the history
//! tables that the verb prunes. It also grants the watermark read and write.
//!
//! Each relation runs in its own transaction. The transaction binds
//! `wamn:audit-retention` as the actor and the operation, and it takes the audit
//! retention lock before it reads `pg_trigger`. apply-package takes the same
//! lock while it changes a log trigger, so the retention that the verb reads
//! holds until its delete commits.
//!
//! The cutoff is the transaction timestamp less n days, in UTC. An entry older
//! than the cutoff goes only when no earlier entry of its row is at or after the
//! cutoff. So the verb removes a prefix of the history of a row, never an
//! interior entry. It advances the pruning watermark in the same transaction.
//! The daily schedule sets the retention precision.
//! The final transaction also expires successful write-log claims after 30 days.
//! Claims with a null result remain, regardless of age.

use sqlx::PgConnection as Client;
use sqlx::{Connection as _, Row as _};
pub(super) mod plan;
use plan::prune_history_sql;

use anyhow::{Context as _, bail};
use postgres_protocol::escape::{escape_identifier, escape_literal};

use wamn_control_provision::audit_retention::{
    AUDIT_RETENTION_LOCK_SQL, AUDIT_RETENTION_TARGETS_SQL, WRITE_LOG_RETENTION_SQL,
    WRITE_LOG_RETENTION_TARGET_SQL,
};
use wamn_control_provision::{
    PlatformComponent, WorkloadRoleFamily, WorkloadRoleIdentity, bind_platform_principal_sql,
    workload_login,
};

/// Inputs of one record history retention run.
#[derive(Debug)]
pub struct PruneRecordHistoryRequest {
    /// Postgres URL for this project's `wamn_audit_retention` credential
    /// generation. The verb refuses any other login.
    pub database_url: String,

    pub org: String,
    pub project: String,
    pub environment: String,
}

impl PruneRecordHistoryRequest {
    /// The composed environment key derived from the environment coordinate.
    pub fn project_env_key(&self) -> String {
        wamn_registry::project_env_key(&self.org, &self.project, &self.environment)
    }
}

/// The result of pruning one history table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrunedHistory {
    /// The schema of the logged relation.
    pub schema: String,
    /// The history table name.
    pub history: String,
    /// The retention of the relation, in whole days.
    pub days: i32,
    /// The number of entries that the delete removed.
    pub removed: u64,
}

/// Prune the expired record history of one project database and return what each
/// history table lost, in the order the run visited the tables.
pub async fn prune_expired_record_history(
    request: PruneRecordHistoryRequest,
) -> anyhow::Result<Vec<PrunedHistory>> {
    if [&request.org, &request.project, &request.environment]
        .iter()
        .any(|part| part.trim().is_empty())
    {
        bail!("org, project and environment must be non-empty");
    }
    let mut client = wamn_postgres::connect(&request.database_url)
        .await
        .context("audit retention credential connect")?;

    let result = async {
        verify_audit_retention_identity(&mut client, &request.project_env_key()).await?;
        prune(&mut client).await
    }
    .await;
    drop(client);
    result
}

/// Refuse unless the connected role is the audit retention login of this exact `(project, database)` pair.
///
/// The verb derives the expected name from the environment coordinate and the server's
/// `current_database()`. The URL is operator input, so the verb does not read
/// the database from it.
async fn verify_audit_retention_identity(
    client: &mut Client,
    project_env_key: &str,
) -> anyhow::Result<()> {
    let row =
        sqlx::query("SELECT current_user::text AS role, current_database()::text AS database")
            .fetch_one(&mut *client)
            .await
            .context("read connected audit retention identity")?;
    let role: String = row.get("role");
    let database: String = row.get("database");
    let target = WorkloadRoleIdentity::ProjectEnvKey {
        project_env_key,
        database: &database,
    };
    let expected = workload_login(WorkloadRoleFamily::AuditRetention, target)
        .context("derive the expected retention identity")?;
    if expected == role {
        return Ok(());
    }
    bail!(
        "refusing to prune record history: connected as {role:?} in database {database:?}, which \
         is not an audit-retention login for project_env_key {project_env_key:?} (expected \
         {expected:?})"
    )
}

/// Prune each history table whose relation keeps its entries for n days.
///
/// The verb visits the relations in byte order of schema and relation. Each
/// transaction binds the platform principal, sets `TimeZone` to UTC, takes the
/// audit retention lock, reads the retention source, and prunes the next
/// relation after the last one that it pruned. The live retention test
/// (`services/ctl/tests/prune_record_history_live.rs`) drives this path
/// through the `wamn-ctl-ops` process.
pub async fn prune(client: &mut Client) -> anyhow::Result<Vec<PrunedHistory>> {
    let mut pruned = Vec::new();
    let mut last: Option<(String, String)> = None;
    loop {
        let mut transaction = client
            .begin()
            .await
            .context("begin a record history retention transaction")?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(bind_platform_principal_sql(
            PlatformComponent::AuditRetention,
        )))
        .execute(&mut *transaction)
        .await
        .map(drop)
        .context("bind wamn:audit-retention as the actor")?;
        sqlx::raw_sql("SET LOCAL TimeZone = 'UTC'")
            .execute(&mut *transaction)
            .await
            .map(drop)
            .context("set the retention time zone")?;
        sqlx::query(AUDIT_RETENTION_LOCK_SQL)
            .fetch_one(&mut *transaction)
            .await
            .context("lock the audit retention source")?;
        let next = sqlx::query(AUDIT_RETENTION_TARGETS_SQL)
            .fetch_all(&mut *transaction)
            .await
            .context("read the retention of each logged relation")?
            .into_iter()
            .map(|row| {
                (
                    row.get::<String, _>("schema_name"),
                    row.get::<String, _>("relation_name"),
                    row.get::<String, _>("history_name"),
                    row.get::<String, _>("days"),
                )
            })
            .find(|(schema, relation, _, _)| {
                last.as_ref().is_none_or(|(last_schema, last_relation)| {
                    (schema, relation) > (last_schema, last_relation)
                })
            });
        let Some((schema, relation, history, days)) = next else {
            if sqlx::query(WRITE_LOG_RETENTION_TARGET_SQL)
                .fetch_optional(&mut *transaction)
                .await
                .context("find the write log retention target")?
                .is_some()
            {
                sqlx::query(WRITE_LOG_RETENTION_SQL)
                    .execute(&mut *transaction)
                    .await
                    .map(|result| result.rows_affected())
                    .context("prune successful write log claims older than 30 days")?;
            }
            transaction
                .commit()
                .await
                .context("end the record history retention run")?;
            return Ok(pruned);
        };
        let days = days.parse::<i32>().with_context(|| {
            format!("the retention P{days}D of {schema}.{relation} has too many days")
        })?;
        let table = format!(
            "{}.{}",
            escape_identifier(&schema),
            escape_identifier(&history)
        );
        let removed: i64 = sqlx::query(sqlx::AssertSqlSafe(prune_history_sql(&table)))
            .bind(days)
            .fetch_one(&mut *transaction)
            .await
            .with_context(|| format!("prune {schema}.{history}"))?
            .get(0);
        transaction
            .commit()
            .await
            .with_context(|| format!("commit the pruning of {schema}.{history}"))?;
        pruned.push(PrunedHistory {
            schema: schema.clone(),
            history,
            days,
            removed: u64::try_from(removed).expect("a deleted row count is nonnegative"),
        });
        last = Some((schema, relation));
    }
}
