//! The SQLite [`IntentStore`] adapter: one file, one writer (docs/plan/edge.md).
//!
//! This crate exports [`SqliteIntentStore`]. It depends on `wamn-run-state` for
//! the trait, so `wamn-run-state` stays adapter-free and the cloud links no
//! SQLite.
//!
//! The file runs in WAL mode with `synchronous=FULL`, so a committed `begin` is
//! on disk before the export runs. `locking_mode=EXCLUSIVE` holds the file for
//! the process that opened it, so a second process cannot write it. Inside the
//! process, one connection behind a mutex is the single writer.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};
use wamn_run_state::IntentStore;
use wamn_run_state::intent_store::{
    Begun, Intent, IntentId, StoreError, StoreErrorKind, StoredOutcome, UncertainIntent,
};
use wamn_run_state::operator_action::OperatorActionBasis;

/// One row per operation call. An intent is open until it finishes or an
/// operator resolves it, and never both.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS intents (
    id INTEGER PRIMARY KEY,
    tenant TEXT NOT NULL,
    release TEXT NOT NULL,
    package TEXT NOT NULL,
    operation TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    input_hash TEXT NOT NULL,
    deadline_ms INTEGER NOT NULL,
    begun_at INTEGER NOT NULL,
    finished_at INTEGER,
    outcome_kind TEXT CHECK (outcome_kind IN ('completed', 'failed')),
    outcome TEXT,
    resolved_basis TEXT,
    resolved_at INTEGER,
    UNIQUE (tenant, idempotency_key),
    CHECK ((finished_at IS NULL) = (outcome_kind IS NULL)),
    CHECK ((outcome_kind IS NULL) = (outcome IS NULL)),
    CHECK ((resolved_basis IS NULL) = (resolved_at IS NULL)),
    CHECK (finished_at IS NULL OR resolved_at IS NULL)
) STRICT;
";

/// An [`IntentStore`] over one SQLite file.
#[derive(Clone, Debug)]
pub struct SqliteIntentStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteIntentStore {
    /// Open or create the intent log at `path` and hold it for this process.
    ///
    /// Fails with [`StoreErrorKind::Storage`] when another process holds the
    /// file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let storage = storage("open");
        let connection = Connection::open(path).map_err(&storage)?;
        // Fail at once, not after a wait, when another process holds the file.
        connection
            .busy_timeout(std::time::Duration::ZERO)
            .map_err(&storage)?;
        connection
            .pragma_update(None, "locking_mode", "EXCLUSIVE")
            .map_err(&storage)?;
        let mode: String = connection
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
            .map_err(&storage)?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::new(
                StoreErrorKind::Storage,
                "open",
                format!("journal mode is {mode}, not wal"),
            ));
        }
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(&storage)?;
        // An exclusive transaction takes the file lock now, and the lock stays
        // until the connection closes.
        connection
            .execute_batch(&format!("BEGIN EXCLUSIVE; {SCHEMA} COMMIT;"))
            .map_err(&storage)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Run `call` on the writer connection, on tokio's blocking pool.
    async fn run<T: Send + 'static>(
        &self,
        operation: &'static str,
        call: impl FnOnce(&mut Connection) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let connection = Arc::clone(&self.connection);
        tokio::task::spawn_blocking(move || {
            let mut connection = connection.lock().map_err(|_| {
                StoreError::new(StoreErrorKind::Storage, operation, "writer lock poisoned")
            })?;
            call(&mut connection)
        })
        .await
        .map_err(|error| StoreError::new(StoreErrorKind::Storage, operation, error.to_string()))?
    }
}

#[async_trait]
impl IntentStore for SqliteIntentStore {
    async fn begin(&self, intent: &Intent<'_>) -> Result<Begun, StoreError> {
        let deadline_ms = i64::try_from(intent.deadline_ms).map_err(|_| {
            contract(
                "begin",
                format!("deadline {} ms does not fit", intent.deadline_ms),
            )
        })?;
        let row = OwnedIntent {
            tenant: intent.tenant.to_owned(),
            release: intent.release.to_owned(),
            package: intent.package.to_owned(),
            operation: intent.operation.to_owned(),
            idempotency_key: intent.idempotency_key.to_owned(),
            input_hash: intent.input_hash.to_owned(),
            deadline_ms,
        };
        self.run("begin", move |connection| begin(connection, &row))
            .await
    }

    async fn finish(&self, id: &IntentId, outcome: &StoredOutcome) -> Result<(), StoreError> {
        let id = row_id("finish", id)?;
        let (kind, value) = match outcome {
            StoredOutcome::Completed(value) => ("completed", value),
            StoredOutcome::Failed(value) => ("failed", value),
        };
        let value = serde_json::to_string(value)
            .map_err(|error| contract("finish", format!("encode outcome: {error}")))?;
        self.run("finish", move |connection| {
            let changed = connection
                .execute(
                    "UPDATE intents SET finished_at = ?1, outcome_kind = ?2, outcome = ?3 \
                     WHERE id = ?4 AND finished_at IS NULL AND resolved_at IS NULL",
                    params![now_ms("finish")?, kind, value, id],
                )
                .map_err(storage("finish"))?;
            if changed == 0 {
                return Err(contract("finish", format!("intent {id} is not open")));
            }
            Ok(())
        })
        .await
    }

    async fn uncertain(&self, limit: u32) -> Result<Vec<UncertainIntent>, StoreError> {
        self.run("uncertain", move |connection| {
            let storage = storage("uncertain");
            let mut statement = connection
                .prepare(
                    "SELECT id, tenant, release, package, operation, idempotency_key \
                     FROM intents WHERE finished_at IS NULL AND resolved_at IS NULL \
                     ORDER BY id LIMIT ?1",
                )
                .map_err(&storage)?;
            let rows = statement
                .query_map([limit], |row| {
                    Ok(UncertainIntent {
                        id: IntentId(row.get::<_, i64>(0)?.to_string()),
                        tenant: row.get(1)?,
                        release: row.get(2)?,
                        package: row.get(3)?,
                        operation: row.get(4)?,
                        idempotency_key: row.get(5)?,
                    })
                })
                .map_err(&storage)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(&storage)
        })
        .await
    }

    async fn resolve(&self, id: &IntentId, basis: OperatorActionBasis) -> Result<(), StoreError> {
        let id = row_id("resolve", id)?;
        self.run("resolve", move |connection| {
            let changed = connection
                .execute(
                    "UPDATE intents SET resolved_basis = ?1, resolved_at = ?2 \
                     WHERE id = ?3 AND finished_at IS NULL AND resolved_at IS NULL",
                    params![basis.as_str(), now_ms("resolve")?, id],
                )
                .map_err(storage("resolve"))?;
            if changed == 0 {
                return Err(contract("resolve", format!("intent {id} is not uncertain")));
            }
            Ok(())
        })
        .await
    }
}

/// The owned fields of one [`Intent`], moved to the blocking pool.
struct OwnedIntent {
    tenant: String,
    release: String,
    package: String,
    operation: String,
    idempotency_key: String,
    input_hash: String,
    deadline_ms: i64,
}

/// Read the key and insert it when it is new, in one committed transaction.
///
/// A resolved intent stays uncertain for `begin`, so its call never runs again.
fn begin(connection: &mut Connection, intent: &OwnedIntent) -> Result<Begun, StoreError> {
    let storage = storage("begin");
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(&storage)?;
    let existing = transaction
        .query_row(
            "SELECT id, input_hash, outcome_kind, outcome FROM intents \
             WHERE tenant = ?1 AND idempotency_key = ?2",
            params![intent.tenant, intent.idempotency_key],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            },
        )
        .optional()
        .map_err(&storage)?;
    let begun = match existing {
        Some((_, input_hash, _, _)) if input_hash != intent.input_hash => {
            return Err(contract(
                "begin",
                format!(
                    "idempotency key {} of tenant {} repeats with a different input",
                    intent.idempotency_key, intent.tenant
                ),
            ));
        }
        Some((_, _, Some(kind), Some(outcome))) => {
            Begun::Finished(stored_outcome(&kind, &outcome)?)
        }
        Some((id, _, _, _)) => Begun::Uncertain(IntentId(id.to_string())),
        None => {
            transaction
                .execute(
                    "INSERT INTO intents (tenant, release, package, operation, idempotency_key, \
                     input_hash, deadline_ms, begun_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        intent.tenant,
                        intent.release,
                        intent.package,
                        intent.operation,
                        intent.idempotency_key,
                        intent.input_hash,
                        intent.deadline_ms,
                        now_ms("begin")?,
                    ],
                )
                .map_err(&storage)?;
            Begun::New(IntentId(transaction.last_insert_rowid().to_string()))
        }
    };
    transaction.commit().map_err(&storage)?;
    Ok(begun)
}

fn stored_outcome(kind: &str, outcome: &str) -> Result<StoredOutcome, StoreError> {
    let value = serde_json::from_str(outcome)
        .map_err(|error| contract("begin", format!("stored outcome is not JSON: {error}")))?;
    match kind {
        "completed" => Ok(StoredOutcome::Completed(value)),
        "failed" => Ok(StoredOutcome::Failed(value)),
        other => Err(contract("begin", format!("stored outcome kind {other}"))),
    }
}

/// The row id that an [`IntentId`] names as decimal text.
fn row_id(operation: &'static str, id: &IntentId) -> Result<i64, StoreError> {
    id.0.parse()
        .map_err(|_| contract(operation, format!("intent id {:?} is not a row id", id.0)))
}

fn now_ms(operation: &'static str) -> Result<i64, StoreError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| StoreError::new(StoreErrorKind::Storage, operation, error.to_string()))?;
    i64::try_from(elapsed.as_millis())
        .map_err(|_| StoreError::new(StoreErrorKind::Storage, operation, "clock out of range"))
}

fn storage(operation: &'static str) -> impl Fn(rusqlite::Error) -> StoreError {
    move |error| StoreError::new(StoreErrorKind::Storage, operation, error.to_string())
}

fn contract(operation: &'static str, detail: String) -> StoreError {
    StoreError::new(StoreErrorKind::Contract, operation, detail)
}
