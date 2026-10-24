//! The write of the `number_counter.set` host route
//! (`docs/plan/platform-config.md`, "Numbering", "Administration").
//!
//! A set runs in an operation transaction of the guest authority, under the
//! caller, the route's operation and the application as claims, so the
//! engine write log records it by its request id and the counter row's
//! `updated_by` names the caller. The set resolves the sequence by the
//! application that the host route checked against the release and the
//! sequence name, never by the `app.application` claim, and passes its id to
//! `platform.set_counter`. That function writes the value under the counter
//! row's lock, so a set waits for a concurrent next number and a next number
//! waits for the set.
//!
//! The set reads the sequence row to find the counter, and reads and writes
//! counter rows only. It renders no number and reads no application table:
//! a set that gives a number a record already carries is caught by that
//! model's `UQ` on the insert that renders it. The set refuses a `per=`
//! counter, whose values are mappings that never change.

use deadpool_postgres::Object;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use wamn_number_pattern::Scope;

use super::super::write_log::OpaqueBegun;
use super::super::{SessionClaims, WamnPostgres};
use super::SEQUENCE_SQL;

/// One set of one counter of a number sequence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CounterSet {
    /// The composing application that owns the sequence.
    pub application: String,
    /// The sequence name.
    pub sequence: String,
    /// The counter name in the sequence's pattern.
    pub counter: String,
    /// The key value of the counter: `''` for a counter with no key, the
    /// field value for `reset=<field>`, the rendered value of the other
    /// counter for `reset=<counter>`, and the date text for `reset=date:FMT`.
    pub key: String,
    /// The next value to render, 1 or more.
    pub value: i64,
}

/// Why a set was refused. The route answers each with its declared literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CounterSetRefusal {
    /// A request member the set cannot take, by its name.
    InvalidInput { field: &'static str },
    /// The application has no sequence of this name.
    SequenceNotFound,
    /// The sequence's pattern has no counter of this name.
    CounterNotFound,
    /// The counter is a `per=` counter.
    AssignedPerKey,
    /// The request id was used for a different set.
    IdempotencyConflict,
}

/// Why a set did not commit.
#[derive(Debug)]
pub enum CounterSetError {
    /// The set was refused and changed nothing.
    Refused(CounterSetRefusal),
    /// The set failed, and changed nothing.
    Failed(anyhow::Error),
}

impl From<CounterSetRefusal> for CounterSetError {
    fn from(refusal: CounterSetRefusal) -> Self {
        Self::Refused(refusal)
    }
}

/// The write of the route.
const SET_SQL: &str = "SELECT platform.set_counter($1::text::uuid, $2, $3, $4)";

impl WamnPostgres {
    /// Set one counter of a sequence of `set.application` for the caller
    /// `user_id`, in one operation transaction of the guest authority of
    /// `tenant` in `project`, logged in the write log under `operation` and
    /// `request_id`. Returns the value the counter renders next. A retry of a
    /// committed set answers its stored value.
    ///
    /// # Errors
    ///
    /// [`CounterSetError::Refused`] for a refused set, and
    /// [`CounterSetError::Failed`] for a failed statement or transaction.
    /// Neither commits anything.
    pub async fn set_number_counter(
        &self,
        project: &str,
        tenant: &str,
        user_id: &str,
        operation: &str,
        request_id: &str,
        set: &CounterSet,
    ) -> Result<i64, CounterSetError> {
        if set.value < 1 {
            return Err(CounterSetRefusal::InvalidInput { field: "value" }.into());
        }
        if request_id.is_empty() {
            return Err(CounterSetRefusal::InvalidInput {
                field: "request_id",
            }
            .into());
        }
        let scope = format!("wamn:number-counter-set:{}", uuid::Uuid::new_v4());
        let claims = SessionClaims {
            tenant: tenant.to_owned(),
            project: Some(project.to_owned()),
            user_id: Some(user_id.to_owned()),
            operation: Some(operation.to_owned()),
            application: Some(set.application.clone()),
            ..SessionClaims::default()
        };
        let result = async {
            self.bind_session_claims(&scope, &claims)
                .await
                .map_err(CounterSetError::Failed)?;
            self.set_in_transaction(&scope, operation, request_id, set)
                .await
        }
        .await;
        self.revoke_session_claims(&scope);
        result
    }

    async fn set_in_transaction(
        &self,
        scope: &str,
        operation: &str,
        request_id: &str,
        set: &CounterSet,
    ) -> Result<i64, CounterSetError> {
        let failed = |context: &str, error: &dyn std::fmt::Debug| {
            CounterSetError::Failed(anyhow::anyhow!("{context}: {error:?}"))
        };
        let transaction = self
            .begin_operation_transaction(scope)
            .await
            .map_err(|error| failed("begin the set's transaction", &error))?;
        let request =
            Sha256::digest(serde_json::to_vec(set).expect("a counter set serializes to JSON"))
                .to_vec();
        let begun = match transaction
            .begin_opaque(operation, request_id, request)
            .await
        {
            Ok(begun) => begun,
            Err(error) => {
                let _ = transaction.rollback().await;
                return Err(failed("claim the request id in the write log", &error));
            }
        };
        let stored = match begun {
            OpaqueBegun::New => None,
            OpaqueBegun::Finished(result) => Some(result),
            OpaqueBegun::Conflict => {
                let _ = transaction.rollback().await;
                return Err(CounterSetRefusal::IdempotencyConflict.into());
            }
            OpaqueBegun::Uncertain => {
                let _ = transaction.rollback().await;
                return Err(CounterSetError::Failed(anyhow::anyhow!(
                    "the write log holds an unfinished claim of request {request_id}"
                )));
            }
        };
        if let Some(result) = stored {
            let _ = transaction.rollback().await;
            return serde_json::from_str::<serde_json::Value>(&result)
                .ok()
                .and_then(|value| value["value"].as_i64())
                .ok_or_else(|| {
                    CounterSetError::Failed(anyhow::anyhow!(
                        "the stored result of request {request_id} names no value"
                    ))
                });
        }
        let outcome = transaction
            .with_host_connection(|connection| async move {
                let result = set_counter(connection.connection(), set).await;
                (connection, result)
            })
            .await;
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(refusal)) => {
                let _ = transaction.rollback().await;
                return Err(refusal.into());
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                return Err(failed("set the counter", &error));
            }
        }
        transaction.finish_opaque(
            operation,
            request_id,
            serde_json::json!({ "value": set.value }).to_string(),
        );
        transaction
            .commit()
            .await
            .map_err(|error| failed("commit the set", &error))?;
        Ok(set.value)
    }
}

/// Set the counter under its row lock. The caller rolls back a refusal.
async fn set_counter(
    connection: &Object,
    set: &CounterSet,
) -> Result<Result<(), CounterSetRefusal>, tokio_postgres::Error> {
    let statement = connection.prepare_cached(SEQUENCE_SQL).await?;
    let row = connection
        .query_one(&statement, &[&set.application, &set.sequence])
        .await?;
    let (Some(sequence), Some(pattern)) = (
        row.get::<_, Option<String>>(0),
        row.get::<_, Option<String>>(1),
    ) else {
        return Ok(Err(CounterSetRefusal::SequenceNotFound));
    };
    // A stored pattern that does not parse has no counter to set.
    let Ok(pattern) = wamn_number_pattern::parse(&pattern) else {
        return Ok(Err(CounterSetRefusal::CounterNotFound));
    };
    let Some(target) = pattern
        .counters()
        .find(|counter| counter.name() == set.counter)
    else {
        return Ok(Err(CounterSetRefusal::CounterNotFound));
    };
    match target.scope() {
        Scope::Per(_) => return Ok(Err(CounterSetRefusal::AssignedPerKey)),
        Scope::Always if !set.key.is_empty() => {
            return Ok(Err(CounterSetRefusal::InvalidInput { field: "key" }));
        }
        Scope::Always | Scope::Reset(_) => {}
    }

    let write = connection.prepare_cached(SET_SQL).await?;
    connection
        .execute(&write, &[&sequence, &set.counter, &set.key, &set.value])
        .await?;

    Ok(Ok(()))
}
