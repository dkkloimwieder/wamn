//! The samples of the device loop, in the run-state file beside the intents.
//!
//! The device operation returns the sample, and the host stores it (spec 4.2).
//! One transaction records the intent outcome and inserts the sample (spec
//! 4.7), so a finished device call always has its sample, and a sample always
//! has its finished intent. A failed call stores no sample. The forward reads
//! the pending samples and records each attempt.

use async_trait::async_trait;
use rusqlite::params;
use serde_json::Value;
use wamn_run_state::IntentStore;
use wamn_run_state::intent_store::{
    Begun, Intent, IntentId, StoreError, StoreErrorKind, StoredOutcome, UncertainIntent,
};
use wamn_run_state::operator_action::OperatorActionBasis;
use wamn_run_state_sqlite::{SqliteIntentStore, finish_in};

/// One row per finished device call. `sample_key` is the key of its intent.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS samples (
    id INTEGER PRIMARY KEY,
    sample_key TEXT NOT NULL UNIQUE,
    intent_id INTEGER NOT NULL UNIQUE REFERENCES intents (id),
    captured_at TEXT NOT NULL,
    body TEXT NOT NULL,
    forwarded_at INTEGER,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT
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

/// The samples table of the run-state file.
#[derive(Debug, Clone)]
pub struct SampleStore {
    store: SqliteIntentStore,
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
        Ok(Self { store })
    }

    /// The intent log of one device call, which stores the call's sample as
    /// captured at `captured_at`.
    pub fn intents<'a>(&'a self, captured_at: &'a str) -> SampleIntents<'a> {
        SampleIntents {
            store: &self.store,
            captured_at,
        }
    }

    /// The oldest samples that are not forwarded, at most `limit`.
    pub async fn pending(&self, limit: u32) -> Result<Vec<Sample>, StoreError> {
        self.store
            .transact("pending", move |transaction| {
                let storage = storage("pending");
                let mut statement = transaction
                    .prepare(
                        "SELECT sample_key, intent_id, captured_at, body, attempts FROM samples \
                         WHERE forwarded_at IS NULL ORDER BY id LIMIT ?1",
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
}

/// The intent log of one device call. A completed finish inserts the sample
/// in the transaction that records the outcome.
pub struct SampleIntents<'a> {
    store: &'a SqliteIntentStore,
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
            .await
    }

    async fn uncertain(&self, limit: u32) -> Result<Vec<UncertainIntent>, StoreError> {
        self.store.uncertain(limit).await
    }

    async fn resolve(&self, id: &IntentId, basis: OperatorActionBasis) -> Result<(), StoreError> {
        self.store.resolve(id, basis).await
    }
}

fn storage(operation: &'static str) -> impl Fn(rusqlite::Error) -> StoreError {
    move |error| StoreError::new(StoreErrorKind::Storage, operation, error.to_string())
}
