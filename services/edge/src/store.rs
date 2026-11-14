//! The edge's durable claims and samples in one SQLx SQLite file.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqliteLockingMode, SqlitePoolOptions,
    SqliteSynchronous,
};
use sqlx::{Row as _, SqlitePool};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS claims (
    id INTEGER PRIMARY KEY,
    partition TEXT NOT NULL,
    operation TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request BLOB NOT NULL,
    begun_at INTEGER NOT NULL,
    outcome TEXT,
    UNIQUE (partition, operation, idempotency_key)
) STRICT;
";

#[derive(Debug)]
struct Writer {
    pool: SqlitePool,
    // Keep process ownership even while SQLx replaces a failed connection.
    _ownership: std::fs::File,
    closed: tokio::sync::watch::Sender<()>,
}

/// One file and one writer, shared by HTTP and device operations.
#[derive(Debug, Clone)]
pub struct EdgeStore {
    writer: Arc<Writer>,
}

/// Wait for all store owners and the SQLite connection to close.
#[derive(Debug)]
pub struct StoreClosed {
    owners: tokio::sync::watch::Receiver<()>,
    pool: SqlitePool,
}

impl StoreClosed {
    pub async fn wait(mut self) {
        let _ = self.owners.changed().await;
        self.pool.close().await;
    }
}

/// The result of claiming a partition, operation and caller key.
#[derive(Debug)]
pub enum Claim {
    New(i64),
    Finished(String),
    InProgress,
    OutcomeUnknown,
    Conflict,
}

impl EdgeStore {
    /// Open the file exclusively. Existing generic intent files are refused.
    pub async fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let ownership = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .context("open edge storage ownership")?;
        ownership
            .try_lock()
            .context("edge storage is already in use")?;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::raw_sql("BEGIN EXCLUSIVE; COMMIT;")
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(path)
                    .create_if_missing(true)
                    .journal_mode(SqliteJournalMode::Wal)
                    .synchronous(SqliteSynchronous::Full)
                    .locking_mode(SqliteLockingMode::Exclusive)
                    .busy_timeout(Duration::ZERO),
            )
            .await
            .context("open edge storage")?;
        let mut connection = pool.acquire().await?;
        let old: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'intents')",
        )
        .fetch_one(&mut *connection)
        .await?;
        anyhow::ensure!(!old, "old edge storage is not supported");
        sqlx::raw_sql(SCHEMA).execute(&mut *connection).await?;
        drop(connection);
        Ok(Self {
            writer: Arc::new(Writer {
                pool,
                _ownership: ownership,
                closed: tokio::sync::watch::Sender::new(()),
            }),
        })
    }

    pub fn closed(&self) -> StoreClosed {
        StoreClosed {
            owners: self.writer.closed.subscribe(),
            pool: self.writer.pool.clone(),
        }
    }

    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.writer.pool
    }

    /// Commit the claim before entering the application.
    pub async fn claim(
        &self,
        partition: &str,
        operation: &str,
        key: &str,
        request: &[u8],
    ) -> anyhow::Result<Claim> {
        let mut transaction = self.pool().begin().await?;
        // Retention far exceeds the maximum call duration, so late completion cannot
        // update a replacement claim. Unfinished claims never hold samples.
        sqlx::query("DELETE FROM claims WHERE outcome IS NULL AND begun_at < ?")
            .bind(now_ms()?.saturating_sub(30 * 24 * 60 * 60 * 1000))
            .execute(&mut *transaction)
            .await?;
        // This first write serializes claims before their lookup.
        let inserted = sqlx::query("INSERT INTO claims (partition, operation, idempotency_key, request, begun_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT (partition, operation, idempotency_key) DO NOTHING")
            .bind(partition).bind(log_operation(operation)).bind(key).bind(request).bind(now_ms()?)
            .execute(&mut *transaction).await?;
        let result = if inserted.rows_affected() == 1 {
            Claim::New(inserted.last_insert_rowid())
        } else {
            let row = sqlx::query("SELECT request, outcome, begun_at FROM claims WHERE partition = ? AND operation = ? AND idempotency_key = ?")
                .bind(partition).bind(log_operation(operation)).bind(key).fetch_one(&mut *transaction).await?;
            if row.get::<Vec<u8>, _>(0) != request {
                Claim::Conflict
            } else if let Some(outcome) = row.get::<Option<String>, _>(1) {
                Claim::Finished(outcome)
            } else if now_ms()?.saturating_sub(row.get(2))
                > i64::try_from(wamn_engine::engine::MAX_HOST_CALL_DURATION.as_millis())?
            {
                Claim::OutcomeUnknown
            } else {
                Claim::InProgress
            }
        };
        transaction.commit().await?;
        Ok(result)
    }

    /// Complete once. A device sample shares this transaction with its outcome.
    pub(crate) async fn complete(
        &self,
        id: i64,
        outcome: &str,
        sample: Option<(&str, &serde_json::Value)>,
    ) -> anyhow::Result<()> {
        let mut transaction = self.pool().begin().await?;
        let changed = sqlx::query("UPDATE claims SET outcome = ? WHERE id = ? AND outcome IS NULL")
            .bind(outcome)
            .bind(id)
            .execute(&mut *transaction)
            .await?
            .rows_affected();
        anyhow::ensure!(changed == 1, "claim {id} is not open");
        if let Some((captured_at, body)) = sample {
            sqlx::query("INSERT INTO samples (sample_key, claim_id, captured_at, body) SELECT idempotency_key, id, ?, ? FROM claims WHERE id = ?")
                .bind(captured_at).bind(serde_json::to_string(body)?).bind(id).execute(&mut *transaction).await?;
        }
        transaction.commit().await?;
        Ok(())
    }

    pub(crate) async fn release(&self, id: i64) -> anyhow::Result<()> {
        let changed = sqlx::query("DELETE FROM claims WHERE id = ? AND outcome IS NULL")
            .bind(id)
            .execute(self.pool())
            .await?
            .rows_affected();
        anyhow::ensure!(changed == 1, "claim {id} is not open");
        Ok(())
    }

    pub(crate) async fn revision(&self) -> anyhow::Result<String> {
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM claims WHERE outcome IS NOT NULL")
                .fetch_one(self.pool())
                .await?;
        Ok(count.to_string())
    }
}

fn log_operation(operation: &str) -> &str {
    operation
        .split_once('@')
        .map_or(operation, |(operation, _)| operation)
}

pub(crate) fn now_ms() -> anyhow::Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::SampleStore;
    use serde_json::json;

    #[tokio::test]
    async fn claims_isolate_operations_and_partitions_and_complete_with_samples() {
        let directory = std::env::temp_dir().join(format!("edge-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("edge.db");
        let store = EdgeStore::open(&path).await.unwrap();
        let closed = store.closed();
        assert!(EdgeStore::open(&path).await.is_err());
        store.pool().acquire().await.unwrap().close().await.unwrap();
        assert!(EdgeStore::open(&path).await.is_err());
        // A replacement must hold the exclusive lock before it enters the pool.
        drop(store.pool().acquire().await.unwrap());
        assert!(EdgeStore::open(&path).await.is_err());
        let Claim::New(id) = store
            .claim("partition", "sample:device/read@1", "key", &[1])
            .await
            .unwrap()
        else {
            panic!("new key is claimable");
        };
        assert!(matches!(
            store
                .claim("partition", "sample:device/read@2", "key", &[1])
                .await
                .unwrap(),
            Claim::InProgress
        ));
        assert!(matches!(
            store
                .claim("partition", "sample:device/read@1", "key", &[2])
                .await
                .unwrap(),
            Claim::Conflict
        ));
        assert!(matches!(
            store
                .claim("partition", "sample:device/other@1", "key", &[1])
                .await
                .unwrap(),
            Claim::New(_)
        ));
        assert!(matches!(
            store
                .claim("other", "sample:device/read@1", "key", &[1])
                .await
                .unwrap(),
            Claim::New(_)
        ));
        let body = json!({"reading": 12.5});
        let result = json!({"value": body}).to_string();
        // No sample table exists yet. The failed insert must undo the completed outcome.
        assert!(
            store
                .complete(id, &result, Some(("capture", &body)))
                .await
                .is_err()
        );
        assert!(matches!(
            store
                .claim("partition", "sample:device/read@1", "key", &[1])
                .await
                .unwrap(),
            Claim::InProgress
        ));
        let samples = SampleStore::open(store.clone()).await.unwrap();
        store
            .complete(id, &result, Some(("capture", &body)))
            .await
            .unwrap();
        assert!(
            store
                .complete(id, &result, Some(("capture", &body)))
                .await
                .is_err()
        );
        drop(samples);
        drop(store);
        closed.wait().await;
        let reopened = EdgeStore::open(&path).await.unwrap();
        let closed = reopened.closed();
        let Claim::Finished(result) = reopened
            .claim("partition", "sample:device/read@1", "key", &[1])
            .await
            .unwrap()
        else {
            panic!("durable outcome replays");
        };
        let result: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["value"]["reading"].as_f64(), Some(12.5));
        let samples = SampleStore::open(reopened.clone()).await.unwrap();
        let pending = samples.pending(10).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].claim, id);
        assert_eq!(pending[0].body["reading"].as_f64(), Some(12.5));
        drop(samples);
        drop(reopened);
        closed.wait().await;
        std::fs::remove_dir_all(directory).unwrap();
    }
}
