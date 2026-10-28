//! The write of the `number_counter.set` host route
//! (docs/architecture/data-access.md#setting-a-counter).
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
    /// The declared application that owns the sequence.
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
pub struct CounterSetError {
    kind: CounterSetErrorKind,
    source: Option<anyhow::Error>,
    context: String,
    set: Box<CounterSet>,
    request_id: String,
}

#[derive(Debug)]
enum CounterSetErrorKind {
    Refused(CounterSetRefusal),
    Failed,
}

impl CounterSetError {
    /// The refusal that the host route translates to its declared literal.
    pub fn refusal(&self) -> Option<&CounterSetRefusal> {
        match &self.kind {
            CounterSetErrorKind::Refused(refusal) => Some(refusal),
            CounterSetErrorKind::Failed => None,
        }
    }

    /// The requested counter and value.
    pub fn set(&self) -> &CounterSet {
        &self.set
    }

    /// The request id that identifies the write log claim.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
}

impl std::fmt::Display for CounterSetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.context)?;
        if let Some(source) = &self.source {
            write!(formatter, ": {source:?}")?;
        }
        write!(
            formatter,
            " (request {:?}, counter {:?})",
            self.request_id, self.set
        )
    }
}

impl std::error::Error for CounterSetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}

fn refused(set: &CounterSet, request_id: &str, refusal: CounterSetRefusal) -> CounterSetError {
    CounterSetError {
        context: format!("the counter set was refused: {refusal:?}"),
        kind: CounterSetErrorKind::Refused(refusal),
        source: None,
        set: Box::new(set.clone()),
        request_id: request_id.to_owned(),
    }
}

fn failed(
    set: &CounterSet,
    request_id: &str,
    context: impl Into<String>,
    source: Option<anyhow::Error>,
) -> CounterSetError {
    CounterSetError {
        kind: CounterSetErrorKind::Failed,
        source,
        context: context.into(),
        set: Box::new(set.clone()),
        request_id: request_id.to_owned(),
    }
}

/// The write of the route.
const SET_SQL: &str = "SELECT platform.set_counter($1::text::uuid, $2, $3, $4)";

impl WamnPostgres {
    /// Set one counter of a sequence of `set.application` for the caller
    /// `user_id`, in one operation transaction of the guest authority of
    /// `env_key` in `project`, logged in the write log under `operation` and
    /// `request_id`. Returns the value the counter renders next. A retry of a
    /// committed set answers its stored value.
    ///
    /// # Errors
    ///
    /// [`CounterSetError::refusal`] identifies a refused set. Other errors
    /// retain the cause of a failed statement or transaction.
    /// Neither commits anything.
    pub async fn set_number_counter(
        &self,
        project: &str,
        env_key: &str,
        user_id: &str,
        operation: &str,
        request_id: &str,
        set: &CounterSet,
    ) -> Result<i64, CounterSetError> {
        if set.value < 1 {
            return Err(refused(
                set,
                request_id,
                CounterSetRefusal::InvalidInput { field: "value" },
            ));
        }
        if request_id.is_empty() {
            return Err(refused(
                set,
                request_id,
                CounterSetRefusal::InvalidInput {
                    field: "request_id",
                },
            ));
        }
        let scope = format!("wamn:number-counter-set:{}", uuid::Uuid::new_v4());
        let claims = SessionClaims {
            env_key: env_key.to_owned(),
            project: Some(project.to_owned()),
            user_id: Some(user_id.to_owned()),
            operation: Some(operation.to_owned()),
            application: Some(set.application.clone()),
            ..SessionClaims::default()
        };
        let result = async {
            self.bind_session_claims(&scope, &claims)
                .await
                .map_err(|error| {
                    failed(
                        set,
                        request_id,
                        "bind the set's session claims",
                        Some(error),
                    )
                })?;
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
        let with_source =
            |context: &str, error: anyhow::Error| failed(set, request_id, context, Some(error));
        let transaction = self
            .begin_operation_transaction(scope)
            .await
            .map_err(|error| with_source("begin the set's transaction", error.into()))?
            .with_known_result(serde_json::json!({ "value": set.value }).to_string());
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
                return Err(with_source(
                    "claim the request id in the write log",
                    error.into(),
                ));
            }
        };
        let stored = match begun {
            OpaqueBegun::New => None,
            OpaqueBegun::Finished(result) => Some(result),
            OpaqueBegun::Conflict => {
                let _ = transaction.rollback().await;
                return Err(refused(
                    set,
                    request_id,
                    CounterSetRefusal::IdempotencyConflict,
                ));
            }
            OpaqueBegun::Uncertain => {
                let _ = transaction.rollback().await;
                return Err(failed(
                    set,
                    request_id,
                    format!("the write log holds an unfinished claim of request {request_id}"),
                    None,
                ));
            }
        };
        if let Some(result) = stored {
            let _ = transaction.rollback().await;
            let context = format!("the stored result of request {request_id} names no value");
            let result = serde_json::from_str::<serde_json::Value>(&result)
                .map_err(|error| with_source(&context, error.into()))?;
            return result["value"]
                .as_i64()
                .ok_or_else(|| failed(set, request_id, context, None));
        }
        let outcome = transaction
            .with_host_connection(|connection| async move {
                let result = set_counter(connection.connection(), set, request_id).await;
                (connection, result)
            })
            .await;
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let _ = transaction.rollback().await;
                return Err(error);
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                return Err(with_source("set the counter", error.into()));
            }
        }
        transaction
            .commit()
            .await
            .map_err(|error| with_source("commit the set", error.into()))?;
        Ok(set.value)
    }
}

/// Set the counter under its row lock. The caller rolls back a refusal.
async fn set_counter(
    connection: &Object,
    set: &CounterSet,
    request_id: &str,
) -> Result<Result<(), CounterSetError>, tokio_postgres::Error> {
    let refused = |refusal| refused(set, request_id, refusal);
    let statement = connection.prepare_cached(SEQUENCE_SQL).await?;
    let row = connection
        .query_one(&statement, &[&set.application, &set.sequence])
        .await?;
    let (Some(sequence), Some(pattern)) = (
        row.get::<_, Option<String>>(0),
        row.get::<_, Option<String>>(1),
    ) else {
        return Ok(Err(refused(CounterSetRefusal::SequenceNotFound)));
    };
    // A stored pattern that does not parse has no counter to set.
    let pattern = match wamn_number_pattern::parse(&pattern) {
        Ok(pattern) => pattern,
        Err(source) => {
            let mut error = refused(CounterSetRefusal::CounterNotFound);
            error.source = Some(source.into());
            return Ok(Err(error));
        }
    };
    let Some(target) = pattern
        .counters()
        .find(|counter| counter.name() == set.counter)
    else {
        return Ok(Err(refused(CounterSetRefusal::CounterNotFound)));
    };
    match target.scope() {
        Scope::Per(_) => return Ok(Err(refused(CounterSetRefusal::AssignedPerKey))),
        Scope::Always if !set.key.is_empty() => {
            return Ok(Err(refused(CounterSetRefusal::InvalidInput {
                field: "key",
            })));
        }
        Scope::Always | Scope::Reset(_) => {}
    }

    let write = connection.prepare_cached(SET_SQL).await?;
    connection
        .execute(&write, &[&sequence, &set.counter, &set.key, &set.value])
        .await?;

    Ok(Ok(()))
}
