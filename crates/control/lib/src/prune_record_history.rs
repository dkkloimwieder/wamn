//! The `prune-record-history` subcommand: the record history retention task.
//!
//! The verb removes expired log entries from the history tables of one tenant
//! database. A relation keeps its entries for n whole days when its
//! `wamn_record_history_log` trigger argument is `P<n>D`, and the verb reads that
//! argument from `pg_trigger`. It skips an `unlimited` relation.
//!
//! **Role:** the verb connects as a scoped `wamn_audit_retention` credential
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
//! The daily schedule
//! (`deploy/platform/audit-retention.example.yaml`) sets the retention precision.
//! The final transaction also expires successful write-log claims after 30 days.
//! Claims with a null result remain, regardless of age.

use anyhow::{Context as _, bail};
use tokio_postgres::{Client, NoTls};
use wamn_control_provision::audit_retention::{
    AUDIT_RETENTION_LOCK_SQL, AUDIT_RETENTION_TARGETS_SQL, WRITE_LOG_RETENTION_SQL,
    WRITE_LOG_RETENTION_TARGET_SQL,
};
use wamn_control_provision::{
    CredentialGeneration, PlatformComponent, WorkloadRoleFamily, WorkloadRoleScope,
    bind_platform_principal_sql, workload_generation_role,
};
use wamn_pg_core::{Identifier, QualifiedName, quote_literal};

/// Inputs of one record history retention run.
#[derive(Debug)]
pub struct PruneRecordHistoryRequest {
    /// Postgres URL for this tenant's `wamn_audit_retention` credential
    /// generation. The verb refuses any other login.
    pub database_url: String,

    pub org: String,
    pub project: String,
    pub environment: String,
}

impl PruneRecordHistoryRequest {
    /// The composed environment key derived from the environment coordinate.
    pub fn env_key(&self) -> String {
        wamn_control_registry::project_env_key(&self.org, &self.project, &self.environment)
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

/// Prune the expired record history of one tenant database and return what each
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
    let (mut client, connection) = tokio_postgres::connect(&request.database_url, NoTls)
        .await
        .context("audit retention credential connect")?;
    let connection_task = tokio::spawn(connection);
    let result = async {
        verify_audit_retention_identity(&client, &request.env_key()).await?;
        prune(&mut client).await
    }
    .await;
    drop(client);
    let _ = connection_task.await;
    result
}

/// Refuse unless the connected role is one of the two audit retention
/// generations of this exact `(tenant, database)` pair.
///
/// The verb derives the expected names from the environment coordinate and the server's
/// `current_database()`. The URL is operator input, so the verb does not read
/// the database from it.
async fn verify_audit_retention_identity(client: &Client, env_key: &str) -> anyhow::Result<()> {
    let row = client
        .query_one(
            "SELECT current_user::text AS role, current_database()::text AS database",
            &[],
        )
        .await
        .context("read connected audit retention identity")?;
    let role: String = row.get("role");
    let database: String = row.get("database");
    let scope = WorkloadRoleScope::EnvKey {
        env_key,
        database: &database,
    };
    let mut expected = Vec::new();
    for generation in [CredentialGeneration::A, CredentialGeneration::B] {
        expected.push(
            workload_generation_role(WorkloadRoleFamily::AuditRetention, scope, generation)
                .context("derive the expected audit retention generation identity")?,
        );
    }
    if expected.contains(&role) {
        return Ok(());
    }
    bail!(
        "refusing to prune record history: connected as {role:?} in database {database:?}, which \
         is not an audit-retention credential generation for env_key {env_key:?} (expected one of \
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
        let transaction = client
            .transaction()
            .await
            .context("begin a record history retention transaction")?;
        transaction
            .batch_execute(&bind_platform_principal_sql(
                PlatformComponent::AuditRetention,
            ))
            .await
            .context("bind wamn:audit-retention as the actor")?;
        transaction
            .batch_execute("SET LOCAL TimeZone = 'UTC'")
            .await
            .context("set the retention time zone")?;
        transaction
            .query_one(AUDIT_RETENTION_LOCK_SQL, &[])
            .await
            .context("lock the audit retention source")?;
        let next = transaction
            .query(AUDIT_RETENTION_TARGETS_SQL, &[])
            .await
            .context("read the retention of each logged relation")?
            .into_iter()
            .map(|row| {
                (
                    row.get::<_, String>("schema_name"),
                    row.get::<_, String>("relation_name"),
                    row.get::<_, String>("history_name"),
                    row.get::<_, String>("days"),
                )
            })
            .find(|(schema, relation, _, _)| {
                last.as_ref().is_none_or(|(last_schema, last_relation)| {
                    (schema, relation) > (last_schema, last_relation)
                })
            });
        let Some((schema, relation, history, days)) = next else {
            if transaction
                .query_opt(WRITE_LOG_RETENTION_TARGET_SQL, &[])
                .await
                .context("find the write log retention target")?
                .is_some()
            {
                transaction
                    .execute(WRITE_LOG_RETENTION_SQL, &[])
                    .await
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
        let table = QualifiedName::new(
            Identifier::new(schema.as_str())?,
            Identifier::new(history.as_str())?,
        )
        .quoted();
        let removed: i64 = transaction
            .query_one(&prune_history_sql(&table), &[&days])
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

/// Delete expired prefixes and advance the watermark in one statement.
///
/// `$1` is the retention in days. An entry goes when its `changed_at` is before
/// the cutoff and no entry of the same row with an earlier position is at or
/// after the cutoff.
fn prune_history_sql(table: &str) -> String {
    format!(
        "WITH removed AS (DELETE FROM {table} AS entry \
          USING (SELECT transaction_timestamp() - make_interval(days => $1) AS at) AS cutoff \
          WHERE entry.changed_at < cutoff.at \
            AND NOT EXISTS (SELECT FROM {table} AS earlier \
                             WHERE earlier.row_key = entry.row_key \
                               AND earlier.position < entry.position \
                               AND earlier.changed_at >= cutoff.at) \
          RETURNING entry.transaction_id), \
          watermark AS (INSERT INTO wamn_history.pruning_watermarks AS current \
                         (history_table, transaction_id) \
                       SELECT {table_literal}::regclass, max(transaction_id) FROM removed \
                       HAVING count(*) > 0 \
                       ON CONFLICT (history_table) DO UPDATE \
                         SET transaction_id = greatest(current.transaction_id, EXCLUDED.transaction_id)) \
          SELECT count(*) FROM removed",
        table_literal = quote_literal(table),
    )
}
