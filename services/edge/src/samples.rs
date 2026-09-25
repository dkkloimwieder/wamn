//! The samples of the device loop, in the run-state file beside the intents.
//!
//! The device operation returns the sample, and the host stores it (spec 4.2).
//! One transaction records the intent outcome and inserts the sample (spec
//! 4.7), so a finished device call always has its sample, and a sample always
//! has its finished intent. A failed call stores no sample.
//!
//! The forward reads the pending samples and records each attempt. A sample
//! that the platform refuses is stored as refused with the platform's reason,
//! and the forward never sends it again. An operator resolves it with
//! `wamn-edge samples resolve`, as an uncertain intent is resolved.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rusqlite::params;
use rusqlite::types::Value as SqlValue;
use serde_json::Value;
use tokio::sync::Notify;
use wamn_run_state::IntentStore;
use wamn_run_state::intent_store::{
    Begun, Intent, IntentId, StoreError, StoreErrorKind, StoredOutcome, UncertainIntent,
};
use wamn_run_state::operator_action::OperatorActionBasis;
use wamn_run_state_sqlite::{SqliteIntentStore, finish_in};

/// One row per finished device call. `sample_key` is the key of its intent.
/// A sample is pending until it is forwarded or refused, and never both. Only
/// a refused sample is resolved, and `last_error` holds the refusal.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS samples (
    id INTEGER PRIMARY KEY,
    sample_key TEXT NOT NULL UNIQUE,
    intent_id INTEGER NOT NULL UNIQUE REFERENCES intents (id),
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
    pub intent: IntentId,
    /// When the host read the frame, in RFC 3339.
    pub captured_at: String,
    /// The `value` of the device operation's item result.
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
    store: SqliteIntentStore,
    /// Fired after each stored sample, so the forward wakes.
    stored: Arc<Notify>,
}

impl SampleStore {
    /// Create the samples table in the file of `store`, when it is missing.
    pub async fn open(store: SqliteIntentStore) -> Result<Self, StoreError> {
        store
            .transact("open samples", |transaction| {
                transaction
                    .execute_batch(SCHEMA)
                    .map_err(storage("open samples"))
            })
            .await?;
        Ok(Self {
            store,
            stored: Arc::default(),
        })
    }

    /// The intent log of one device call, which stores the call's sample as
    /// captured at `captured_at`.
    pub fn intents<'a>(&'a self, captured_at: &'a str) -> SampleIntents<'a> {
        SampleIntents {
            store: &self.store,
            stored: &self.stored,
            captured_at,
        }
    }

    /// Wait until a sample is stored. A sample stored since the last wait
    /// ends the wait at once.
    pub async fn wait_stored(&self) {
        self.stored.notified().await;
    }

    /// The oldest pending samples, at most `limit`. A pending sample is
    /// neither forwarded nor refused.
    pub async fn pending(&self, limit: u32) -> Result<Vec<Sample>, StoreError> {
        self.store
            .transact("pending", move |transaction| {
                let storage = storage("pending");
                let mut statement = transaction
                    .prepare(
                        "SELECT sample_key, intent_id, captured_at, body, attempts FROM samples \
                         WHERE forwarded_at IS NULL AND refused_at IS NULL ORDER BY id LIMIT ?1",
                    )
                    .map_err(&storage)?;
                let rows = statement
                    .query_map([limit], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, u32>(4)?,
                        ))
                    })
                    .map_err(&storage)?;
                rows.map(|row| {
                    let (sample_key, intent, captured_at, body, attempts) =
                        row.map_err(&storage)?;
                    Ok(Sample {
                        sample_key,
                        intent: IntentId(intent.to_string()),
                        captured_at,
                        body: serde_json::from_str(&body).map_err(|error| {
                            StoreError::new(
                                StoreErrorKind::Contract,
                                "pending",
                                format!("stored sample is not JSON: {error}"),
                            )
                        })?,
                        attempts,
                    })
                })
                .collect()
            })
            .await
    }

    /// Record that the platform accepted the pending sample `sample_key`.
    pub async fn forwarded(&self, sample_key: &str) -> Result<(), StoreError> {
        let now = now_ms("forwarded")?;
        self.update_pending(
            "forwarded",
            "UPDATE samples SET forwarded_at = ?2, attempts = attempts + 1",
            sample_key,
            vec![now.into()],
        )
        .await
    }

    /// Record that the platform refused the pending sample `sample_key` for
    /// `reason`. The forward never sends it again.
    pub async fn refused(&self, sample_key: &str, reason: &str) -> Result<(), StoreError> {
        let now = now_ms("refused")?;
        self.update_pending(
            "refused",
            "UPDATE samples SET refused_at = ?2, last_error = ?3, attempts = attempts + 1",
            sample_key,
            vec![now.into(), reason.to_owned().into()],
        )
        .await
    }

    /// Record a forward attempt of `sample_key` that did not reach the
    /// platform. The sample stays pending.
    pub async fn attempt_failed(&self, sample_key: &str, error: &str) -> Result<(), StoreError> {
        self.update_pending(
            "attempt failed",
            "UPDATE samples SET last_error = ?2, attempts = attempts + 1",
            sample_key,
            vec![error.to_owned().into()],
        )
        .await
    }

    /// Run `update` on the pending sample `sample_key`, which is `?1`, with
    /// `values` as `?2` onward.
    async fn update_pending(
        &self,
        operation: &'static str,
        update: &'static str,
        sample_key: &str,
        values: Vec<SqlValue>,
    ) -> Result<(), StoreError> {
        let sample_key = sample_key.to_owned();
        self.store
            .transact(operation, move |transaction| {
                let changed = transaction
                    .execute(
                        &format!(
                            "{update} WHERE sample_key = ?1 AND forwarded_at IS NULL \
                             AND refused_at IS NULL"
                        ),
                        rusqlite::params_from_iter(
                            std::iter::once(SqlValue::Text(sample_key.clone())).chain(values),
                        ),
                    )
                    .map_err(storage(operation))?;
                if changed == 0 {
                    return Err(StoreError::new(
                        StoreErrorKind::Contract,
                        operation,
                        format!("sample {sample_key} is not pending"),
                    ));
                }
                Ok(())
            })
            .await
    }

    /// The refused samples that no operator resolved, oldest first.
    pub async fn refused_samples(&self) -> Result<Vec<RefusedSample>, StoreError> {
        self.store
            .transact("refused samples", |transaction| {
                let storage = storage("refused samples");
                let mut statement = transaction
                    .prepare(
                        "SELECT sample_key, captured_at, last_error, attempts FROM samples \
                         WHERE refused_at IS NOT NULL AND resolved_at IS NULL ORDER BY id",
                    )
                    .map_err(&storage)?;
                let rows = statement
                    .query_map([], |row| {
                        Ok(RefusedSample {
                            sample_key: row.get(0)?,
                            captured_at: row.get(1)?,
                            reason: row.get(2)?,
                            attempts: row.get(3)?,
                        })
                    })
                    .map_err(&storage)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(&storage)
            })
            .await
    }

    /// Close the refused sample `sample_key` by an operator decision. It is
    /// never forwarded.
    pub async fn resolve(
        &self,
        sample_key: &str,
        basis: OperatorActionBasis,
    ) -> Result<(), StoreError> {
        let now = now_ms("resolve sample")?;
        let sample_key = sample_key.to_owned();
        self.store
            .transact("resolve sample", move |transaction| {
                let changed = transaction
                    .execute(
                        "UPDATE samples SET resolved_basis = ?2, resolved_at = ?3 \
                         WHERE sample_key = ?1 AND refused_at IS NOT NULL AND resolved_at IS NULL",
                        params![sample_key, basis.as_str(), now],
                    )
                    .map_err(storage("resolve sample"))?;
                if changed == 0 {
                    return Err(StoreError::new(
                        StoreErrorKind::Contract,
                        "resolve sample",
                        format!("sample {sample_key} is not refused and open"),
                    ));
                }
                Ok(())
            })
            .await
    }
}

/// The intent log of one device call. A completed finish inserts the sample
/// in the transaction that records the outcome.
pub struct SampleIntents<'a> {
    store: &'a SqliteIntentStore,
    stored: &'a Notify,
    captured_at: &'a str,
}

impl std::fmt::Debug for SampleIntents<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SampleIntents")
            .field("captured_at", &self.captured_at)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl IntentStore for SampleIntents<'_> {
    async fn begin(&self, intent: &Intent<'_>) -> Result<Begun, StoreError> {
        self.store.begin(intent).await
    }

    async fn finish(&self, id: &IntentId, outcome: &StoredOutcome) -> Result<(), StoreError> {
        let StoredOutcome::Completed(result) = outcome else {
            return self.store.finish(id, outcome).await;
        };
        let body = result.get("value").ok_or_else(|| {
            StoreError::new(
                StoreErrorKind::Contract,
                "finish",
                "the device operation returned an item without a value",
            )
        })?;
        let body = body.to_string();
        let (id, outcome) = (id.clone(), outcome.clone());
        let captured_at = self.captured_at.to_owned();
        self.store
            .transact("finish", move |transaction| {
                finish_in(transaction, &id, &outcome)?;
                transaction
                    .execute(
                        "INSERT INTO samples (sample_key, intent_id, captured_at, body) \
                         SELECT idempotency_key, id, ?2, ?3 FROM intents WHERE id = ?1",
                        params![id.0, captured_at, body],
                    )
                    .map_err(storage("finish"))?;
                Ok(())
            })
            .await?;
        self.stored.notify_one();
        Ok(())
    }

    async fn uncertain(&self, limit: u32) -> Result<Vec<UncertainIntent>, StoreError> {
        self.store.uncertain(limit).await
    }

    async fn resolve(&self, id: &IntentId, basis: OperatorActionBasis) -> Result<(), StoreError> {
        self.store.resolve(id, basis).await
    }
}

fn now_ms(operation: &'static str) -> Result<i64, StoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .ok_or_else(|| StoreError::new(StoreErrorKind::Storage, operation, "clock out of range"))
}

fn storage(operation: &'static str) -> impl Fn(rusqlite::Error) -> StoreError {
    move |error| StoreError::new(StoreErrorKind::Storage, operation, error.to_string())
}
