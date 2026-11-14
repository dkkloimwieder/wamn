//! Device samples share the transaction that completes their operation claim.

use crate::store::{EdgeStore, now_ms};
use serde_json::Value;
use sqlx::Row as _;
use std::sync::Arc;
use tokio::sync::Notify;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS samples (
    id INTEGER PRIMARY KEY,
    sample_key TEXT NOT NULL UNIQUE,
    claim_id INTEGER NOT NULL UNIQUE REFERENCES claims (id),
    captured_at TEXT NOT NULL,
    body TEXT NOT NULL,
    forwarded_at INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    refused_at INTEGER,
    resolved_basis TEXT,
    resolved_at INTEGER,
    CHECK (forwarded_at IS NULL OR refused_at IS NULL),
    CHECK (refused_at IS NULL OR last_error IS NOT NULL),
    CHECK ((resolved_basis IS NULL) = (resolved_at IS NULL)),
    CHECK (resolved_at IS NULL OR refused_at IS NOT NULL)
) STRICT;
";

/// One stored sample.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub sample_key: String,
    pub claim: i64,
    /// When the host read the frame, in RFC 3339.
    pub captured_at: String,
    /// The `value` of the device operation's request element result.
    pub body: Value,
    /// The forward attempts so far.
    pub attempts: u32,
}

/// A sample that the platform refused and no operator resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedSample {
    pub sample_key: String,
    pub captured_at: String,
    /// The platform's reason.
    pub reason: String,
    pub attempts: u32,
}

/// The samples table of the run-state file.
#[derive(Debug, Clone)]
pub struct SampleStore {
    store: EdgeStore,
    /// Fired after each stored sample, so the forward wakes.
    stored: Arc<Notify>,
}

impl SampleStore {
    pub async fn open(store: EdgeStore) -> anyhow::Result<Self> {
        sqlx::raw_sql(SCHEMA).execute(store.pool()).await?;
        Ok(Self {
            store,
            stored: Arc::default(),
        })
    }

    pub(crate) fn notify_stored(&self) {
        self.stored.notify_one();
    }
    pub async fn wait_stored(&self) {
        self.stored.notified().await;
    }

    pub async fn pending_count(&self) -> anyhow::Result<u32> {
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM samples WHERE forwarded_at IS NULL AND refused_at IS NULL",
        )
        .fetch_one(self.store.pool())
        .await?)
    }

    pub async fn pending(&self, limit: u32) -> anyhow::Result<Vec<Sample>> {
        sqlx::query("SELECT sample_key, claim_id, captured_at, body, attempts FROM samples WHERE forwarded_at IS NULL AND refused_at IS NULL ORDER BY id LIMIT ?")
            .bind(limit).fetch_all(self.store.pool()).await?.into_iter().map(|row| Ok(Sample {
                sample_key: row.try_get(0)?, claim: row.try_get(1)?, captured_at: row.try_get(2)?,
                body: serde_json::from_str(row.try_get(3)?)?, attempts: row.try_get(4)?,
            })).collect()
    }

    pub async fn forwarded(&self, key: &str) -> anyhow::Result<()> {
        self.update(key, "UPDATE samples SET forwarded_at = ?, attempts = attempts + 1 WHERE sample_key = ? AND forwarded_at IS NULL AND refused_at IS NULL", None).await
    }
    pub async fn refused(&self, key: &str, reason: &str) -> anyhow::Result<()> {
        self.update(key, "UPDATE samples SET refused_at = ?, last_error = ?, attempts = attempts + 1 WHERE sample_key = ? AND forwarded_at IS NULL AND refused_at IS NULL", Some(reason)).await
    }
    pub async fn attempt_failed(&self, key: &str, error: &str) -> anyhow::Result<()> {
        let changed = sqlx::query("UPDATE samples SET last_error = ?, attempts = attempts + 1 WHERE sample_key = ? AND forwarded_at IS NULL AND refused_at IS NULL")
            .bind(error).bind(key).execute(self.store.pool()).await?.rows_affected();
        anyhow::ensure!(changed == 1, "sample {key} is not pending");
        Ok(())
    }
    async fn update(
        &self,
        key: &str,
        sql: &'static str,
        reason: Option<&str>,
    ) -> anyhow::Result<()> {
        let query = sqlx::query(sql).bind(now_ms()?);
        let query = match reason {
            Some(reason) => query.bind(reason),
            None => query,
        };
        let changed = query
            .bind(key)
            .execute(self.store.pool())
            .await?
            .rows_affected();
        anyhow::ensure!(changed == 1, "sample {key} is not pending");
        Ok(())
    }
    pub async fn refused_samples(&self) -> anyhow::Result<Vec<RefusedSample>> {
        Ok(sqlx::query("SELECT sample_key, captured_at, last_error, attempts FROM samples WHERE refused_at IS NOT NULL AND resolved_at IS NULL ORDER BY id")
            .fetch_all(self.store.pool()).await?.into_iter().map(|row| RefusedSample {
                sample_key: row.get(0), captured_at: row.get(1), reason: row.get(2), attempts: row.get(3),
            }).collect())
    }
    pub async fn resolve(&self, key: &str, basis: OperatorDecision) -> anyhow::Result<()> {
        let changed = sqlx::query("UPDATE samples SET resolved_basis = ?, resolved_at = ? WHERE sample_key = ? AND refused_at IS NOT NULL AND resolved_at IS NULL")
            .bind(basis.as_str()).bind(now_ms()?).bind(key).execute(self.store.pool()).await?.rows_affected();
        anyhow::ensure!(changed == 1, "sample {key} is not refused and open");
        Ok(())
    }
}

/// The evidence an operator used to close a refused sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperatorDecision {
    ExternalEvidence,
    CounterpartyConfirmation,
    OperatorJudgment,
}
impl OperatorDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExternalEvidence => "external-evidence",
            Self::CounterpartyConfirmation => "counterparty-confirmation",
            Self::OperatorJudgment => "operator-judgment",
        }
    }
}
impl std::fmt::Display for OperatorDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl std::str::FromStr for OperatorDecision {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> anyhow::Result<Self> {
        match value {
            "external-evidence" => Ok(Self::ExternalEvidence),
            "counterparty-confirmation" => Ok(Self::CounterpartyConfirmation),
            "operator-judgment" => Ok(Self::OperatorJudgment),
            _ => anyhow::bail!("unknown sample operator decision: {value}"),
        }
    }
}
