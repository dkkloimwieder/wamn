//! Read-only PostgreSQL observation for stored DbState assertions.

use anyhow::Context as _;
use tokio_postgres::Client;
use tokio_postgres::error::SqlState;

use wamn_scenario_model::{Assertion, DbCapture, TestCase};

/// Stable classification for a rejected stored DbState observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DbStateCaptureFailureKind {
    /// PostgreSQL rejected a mutating statement in the read-only transaction.
    ReadOnlyViolation,
    /// PostgreSQL rejected an attempt to change or misuse transaction state.
    TransactionControl,
}

impl DbStateCaptureFailureKind {
    fn message(self) -> &'static str {
        match self {
            Self::ReadOnlyViolation => "db-state observation rejected a write",
            Self::TransactionControl => "db-state observation rejected transaction control",
        }
    }
}

/// A stored DbState query was rejected by the observation transaction boundary.
#[derive(Debug)]
pub struct DbStateCaptureFailure {
    kind: DbStateCaptureFailureKind,
    source: tokio_postgres::Error,
}

impl DbStateCaptureFailure {
    /// The stable failure classification.
    pub fn kind(&self) -> DbStateCaptureFailureKind {
        self.kind
    }

    fn from_postgres(source: tokio_postgres::Error) -> Result<Self, tokio_postgres::Error> {
        match source.code().and_then(classify_sqlstate) {
            Some(kind) => Ok(Self { kind, source }),
            None => Err(source),
        }
    }
}

impl std::fmt::Display for DbStateCaptureFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.kind.message())
    }
}

impl std::error::Error for DbStateCaptureFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn classify_sqlstate(code: &SqlState) -> Option<DbStateCaptureFailureKind> {
    if code == &SqlState::READ_ONLY_SQL_TRANSACTION {
        Some(DbStateCaptureFailureKind::ReadOnlyViolation)
    } else if code.code().starts_with("25") {
        Some(DbStateCaptureFailureKind::TransactionControl)
    } else {
        None
    }
}

enum ObservationError<E> {
    Query(E),
    Rollback(E),
}

fn finish_observation<T, E>(
    query_result: Result<T, E>,
    rollback_result: Result<(), E>,
) -> Result<T, ObservationError<E>> {
    match query_result {
        Err(error) => Err(ObservationError::Query(error)),
        Ok(rows) => rollback_result
            .map(|()| rows)
            .map_err(ObservationError::Rollback),
    }
}

/// Capture stored DbState assertions through explicit read-only transactions.
///
/// The caller supplies its already tenant/schema-scoped scenario application
/// identity. Every assertion gets a fresh transaction and an explicit rollback.
/// Keeping assertions transaction-isolated also contains stored `COMMIT`,
/// `ROLLBACK`, and `SET TRANSACTION` statements: none can change the mode or
/// state used by the next assertion.
pub async fn capture_db_assertions(
    client: &mut Client,
    case: &TestCase,
) -> anyhow::Result<Vec<DbCapture>> {
    let mut captures = Vec::new();
    for assertion in &case.expect {
        let Assertion::DbState { query, params, .. } = assertion else {
            continue;
        };
        let owned: Vec<String> = params
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string())
            })
            .collect();
        let references: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = owned
            .iter()
            .map(|value| value as &(dyn tokio_postgres::types::ToSql + Sync))
            .collect();
        let transaction = client
            .build_transaction()
            .read_only(true)
            .start()
            .await
            .with_context(|| format!("start db-state observation for {}", case.name))?;
        let query_result = transaction.query(query, &references).await;
        let rollback_result = transaction.rollback().await;
        let rows = match finish_observation(query_result, rollback_result) {
            Ok(rows) => rows,
            Err(ObservationError::Query(error)) => {
                match DbStateCaptureFailure::from_postgres(error) {
                    Ok(failure) => return Err(failure.into()),
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("capture db-state assertion for {}", case.name)
                        });
                    }
                }
            }
            Err(ObservationError::Rollback(error)) => {
                return Err(error)
                    .with_context(|| format!("rollback db-state observation for {}", case.name));
            }
        };
        captures.push(DbCapture {
            query: query.clone(),
            params: params.clone(),
            rows: rows
                .iter()
                .map(|row| row.get::<usize, serde_json::Value>(0))
                .collect(),
        });
    }
    Ok(captures)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_failure_displays_do_not_echo_sensitive_input() {
        assert_eq!(
            DbStateCaptureFailureKind::ReadOnlyViolation.message(),
            "db-state observation rejected a write"
        );
        assert_eq!(
            DbStateCaptureFailureKind::TransactionControl.message(),
            "db-state observation rejected transaction control"
        );
    }

    #[test]
    fn transaction_sqlstates_are_classified_intentionally() {
        assert_eq!(
            classify_sqlstate(&SqlState::READ_ONLY_SQL_TRANSACTION),
            Some(DbStateCaptureFailureKind::ReadOnlyViolation)
        );
        assert_eq!(
            classify_sqlstate(&SqlState::ACTIVE_SQL_TRANSACTION),
            Some(DbStateCaptureFailureKind::TransactionControl)
        );
        assert_eq!(classify_sqlstate(&SqlState::SYNTAX_ERROR), None);
    }

    #[test]
    fn query_rejection_takes_precedence_over_rollback_failure() {
        let result = finish_observation::<(), _>(Err("query"), Err("rollback"));
        assert!(matches!(result, Err(ObservationError::Query("query"))));
    }
}
