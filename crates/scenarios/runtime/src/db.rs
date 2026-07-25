//! Read-only PostgreSQL observation for stored DbState assertions.

use std::time::Duration;

use futures_util::TryStreamExt as _;
use tokio_postgres::Client;
use tokio_postgres::error::SqlState;
use tokio_postgres::types::Type;

use wamn_scenario_model::{Assertion, DbCapture, TestCase};

const SET_STATEMENT_TIMEOUT_SQL: &str = "SELECT set_config('statement_timeout', $1, true)";
const NESTED_DATA_MODIFYING_CTE_MESSAGE: &str =
    "WITH clause containing a data-modifying statement must be at the top level";

/// Fixed resource limits for one stored DbState assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DbStateCaptureLimits {
    statement_timeout_ms: u32,
    max_rows: u32,
    max_json_bytes: u32,
}

impl DbStateCaptureLimits {
    /// Current exploratory-development safety rails.
    ///
    /// The checked-in POC suites contain ten DbState assertions, each expecting
    /// at most one row; their largest declared first-row JSON value is 34 bytes.
    /// These limits leave substantial development headroom without making a
    /// production capacity promise.
    pub const POC: Self = Self {
        statement_timeout_ms: 5_000,
        max_rows: 256,
        max_json_bytes: 1024 * 1024,
    };

    /// Build a finite, non-zero limit set.
    ///
    /// PostgreSQL represents these settings and counters as signed 32-bit
    /// values at the relevant boundaries, so larger values are rejected.
    pub fn new(statement_timeout: Duration, max_rows: u32, max_json_bytes: u32) -> Option<Self> {
        let statement_timeout_ms = u32::try_from(statement_timeout.as_millis()).ok()?;
        let postgres_max = i32::MAX.unsigned_abs();
        if statement_timeout_ms == 0
            || statement_timeout_ms > postgres_max
            || max_rows == 0
            || max_rows > postgres_max
            || max_json_bytes == 0
            || max_json_bytes > postgres_max
        {
            return None;
        }
        Some(Self {
            statement_timeout_ms,
            max_rows,
            max_json_bytes,
        })
    }

    /// PostgreSQL statement timeout applied while preparing and running an assertion.
    pub fn statement_timeout(self) -> Duration {
        Duration::from_millis(u64::from(self.statement_timeout_ms))
    }

    /// Maximum rows materialized for one assertion.
    pub fn max_rows(self) -> u32 {
        self.max_rows
    }

    /// Maximum serialized JSON bytes materialized for one assertion.
    pub fn max_json_bytes(self) -> u32 {
        self.max_json_bytes
    }
}

impl Default for DbStateCaptureLimits {
    fn default() -> Self {
        Self::POC
    }
}

/// Stable classification for a rejected stored DbState observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DbStateCaptureFailureKind {
    /// PostgreSQL rejected a mutating statement in the read-only transaction.
    ReadOnlyViolation,
    /// PostgreSQL rejected an attempt to change or misuse transaction state.
    TransactionControl,
    /// PostgreSQL stopped the assertion at its configured statement timeout.
    StatementTimeout,
    /// The assertion returned more rows than the configured materialization limit.
    RowLimit,
    /// The assertion returned more serialized JSON than the configured byte limit.
    ByteLimit,
    /// The assertion was cancelled by a caller or database operator.
    Cancelled,
    /// The PostgreSQL dependency disappeared while the assertion was executing.
    DependencyUnavailable,
    /// The assertion did not return exactly one non-null JSON or JSONB column.
    ResultShape,
}

impl DbStateCaptureFailureKind {
    fn message(self) -> &'static str {
        match self {
            Self::ReadOnlyViolation => "db-state observation rejected a write",
            Self::TransactionControl => "db-state observation rejected transaction control",
            Self::StatementTimeout => "db-state observation exceeded its statement timeout",
            Self::RowLimit => "db-state observation exceeded its row limit",
            Self::ByteLimit => "db-state observation exceeded its JSON byte limit",
            Self::Cancelled => "db-state observation was cancelled",
            Self::DependencyUnavailable => "db-state observation lost its PostgreSQL dependency",
            Self::ResultShape => "db-state observation must return one non-null JSON column",
        }
    }
}

/// A stored DbState query was rejected by its observation boundary.
pub struct DbStateCaptureFailure {
    kind: DbStateCaptureFailureKind,
    source: Option<tokio_postgres::Error>,
}

impl DbStateCaptureFailure {
    /// The stable failure classification.
    pub fn kind(&self) -> DbStateCaptureFailureKind {
        self.kind
    }

    fn local(kind: DbStateCaptureFailureKind) -> Self {
        Self { kind, source: None }
    }

    fn from_postgres(source: tokio_postgres::Error) -> Result<Self, tokio_postgres::Error> {
        match classify_postgres_error(&source) {
            Some(kind) => Ok(Self {
                kind,
                source: Some(source),
            }),
            None => Err(source),
        }
    }
}

impl std::fmt::Debug for DbStateCaptureFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DbStateCaptureFailure")
            .field("kind", &self.kind)
            .finish()
    }
}

impl std::fmt::Display for DbStateCaptureFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.kind.message())
    }
}

impl std::error::Error for DbStateCaptureFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

fn classify_postgres_error(error: &tokio_postgres::Error) -> Option<DbStateCaptureFailureKind> {
    if error.is_closed() {
        return Some(DbStateCaptureFailureKind::DependencyUnavailable);
    }
    let db_error = error.as_db_error()?;
    classify_sqlstate(db_error.code(), db_error.message())
}

fn classify_sqlstate(code: &SqlState, message: &str) -> Option<DbStateCaptureFailureKind> {
    if code == &SqlState::READ_ONLY_SQL_TRANSACTION {
        Some(DbStateCaptureFailureKind::ReadOnlyViolation)
    } else if code.code().starts_with("25") {
        Some(DbStateCaptureFailureKind::TransactionControl)
    } else if code == &SqlState::QUERY_CANCELED {
        if message.contains("statement timeout") {
            Some(DbStateCaptureFailureKind::StatementTimeout)
        } else {
            Some(DbStateCaptureFailureKind::Cancelled)
        }
    } else if code.code().starts_with("08") || code.code().starts_with("57P") {
        Some(DbStateCaptureFailureKind::DependencyUnavailable)
    } else {
        None
    }
}

enum ObservationError<Q, R> {
    Query(Q),
    Rollback(R),
}

fn finish_observation<T, Q, R>(
    query_result: Result<T, Q>,
    rollback_result: Result<(), R>,
) -> Result<T, ObservationError<Q, R>> {
    match query_result {
        Err(error) => Err(ObservationError::Query(error)),
        Ok(rows) => rollback_result
            .map(|()| rows)
            .map_err(ObservationError::Rollback),
    }
}

fn postgres_failure(
    error: tokio_postgres::Error,
    context: impl FnOnce() -> String,
) -> anyhow::Error {
    match DbStateCaptureFailure::from_postgres(error) {
        Ok(failure) => failure.into(),
        Err(error) => anyhow::Error::new(error).context(context()),
    }
}

fn bounded_postgres_failure(
    error: tokio_postgres::Error,
    context: impl FnOnce() -> String,
) -> anyhow::Error {
    // The original statement has already prepared successfully. This exact
    // parser error can therefore only be introduced by placing its
    // data-modifying CTE inside the resource-bounding wrapper.
    let wrapper_rejected_write = error.as_db_error().is_some_and(|db_error| {
        is_bounded_wrapper_write_rejection(db_error.code(), db_error.message())
    });
    if wrapper_rejected_write {
        return DbStateCaptureFailure {
            kind: DbStateCaptureFailureKind::ReadOnlyViolation,
            source: Some(error),
        }
        .into();
    }
    postgres_failure(error, context)
}

fn is_bounded_wrapper_write_rejection(code: &SqlState, message: &str) -> bool {
    code == &SqlState::FEATURE_NOT_SUPPORTED && message == NESTED_DATA_MODIFYING_CTE_MESSAGE
}

fn local_failure(kind: DbStateCaptureFailureKind) -> anyhow::Error {
    DbStateCaptureFailure::local(kind).into()
}

fn bounded_observation_sql(
    query: &str,
    parameter_count: usize,
) -> Result<String, DbStateCaptureFailureKind> {
    let row_limit_parameter = parameter_count
        .checked_add(1)
        .ok_or(DbStateCaptureFailureKind::ResultShape)?;
    let byte_limit_parameter = parameter_count
        .checked_add(2)
        .ok_or(DbStateCaptureFailureKind::ResultShape)?;
    let query = query.trim();
    let query = query.strip_suffix(';').map(str::trim_end).unwrap_or(query);

    // The stored query is intentionally the executable body of this read-only,
    // least-privileged transaction. The wrapper does not interpolate a value or
    // identifier: it narrows the supported one-JSON-column result before bytes
    // cross the PostgreSQL protocol boundary.
    Ok(format!(
        "WITH wamn_db_state(wamn_value) AS (\n{query}\n), \
         wamn_bounded AS ( \
             SELECT wamn_value::text AS json_text \
             FROM wamn_db_state \
             LIMIT ${row_limit_parameter}::text::bigint \
         ) \
         SELECT \
             CASE \
                 WHEN octet_length(json_text)::bigint \
                      <= ${byte_limit_parameter}::text::bigint \
                 THEN json_text \
             END AS json_text, \
             octet_length(json_text)::bigint AS json_bytes \
         FROM wamn_bounded"
    ))
}

#[derive(Debug)]
struct MaterializationBudget {
    rows: usize,
    json_bytes: usize,
    max_rows: usize,
    max_json_bytes: usize,
}

impl MaterializationBudget {
    fn new(limits: DbStateCaptureLimits) -> Self {
        Self {
            rows: 0,
            json_bytes: 0,
            max_rows: usize::try_from(limits.max_rows)
                .expect("u32 fits usize on supported Rust targets"),
            max_json_bytes: usize::try_from(limits.max_json_bytes)
                .expect("u32 fits usize on supported Rust targets"),
        }
    }

    fn begin_row(&mut self) -> Result<(), DbStateCaptureFailureKind> {
        let next = self
            .rows
            .checked_add(1)
            .ok_or(DbStateCaptureFailureKind::RowLimit)?;
        if next > self.max_rows {
            return Err(DbStateCaptureFailureKind::RowLimit);
        }
        self.rows = next;
        Ok(())
    }

    fn add_json_bytes(&mut self, row_bytes: usize) -> Result<(), DbStateCaptureFailureKind> {
        let next = self
            .json_bytes
            .checked_add(row_bytes)
            .ok_or(DbStateCaptureFailureKind::ByteLimit)?;
        if next > self.max_json_bytes {
            return Err(DbStateCaptureFailureKind::ByteLimit);
        }
        self.json_bytes = next;
        Ok(())
    }
}

async fn capture_observation(
    transaction: &tokio_postgres::Transaction<'_>,
    case_name: &str,
    query: &str,
    params: &[String],
    limits: DbStateCaptureLimits,
) -> anyhow::Result<Vec<serde_json::Value>> {
    let timeout = format!("{}ms", limits.statement_timeout_ms);
    transaction
        .query_one(SET_STATEMENT_TIMEOUT_SQL, &[&timeout])
        .await
        .map_err(|error| {
            postgres_failure(error, || {
                format!("set db-state statement timeout for {case_name}")
            })
        })?;

    let statement = transaction.prepare(query).await.map_err(|error| {
        postgres_failure(error, || {
            format!("prepare db-state assertion for {case_name}")
        })
    })?;
    if statement.params().len() != params.len() {
        return Err(local_failure(DbStateCaptureFailureKind::ResultShape));
    }

    let references: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
        .iter()
        .map(|value| value as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();

    if statement.columns().is_empty() {
        let stream = transaction
            .query_raw(&statement, references)
            .await
            .map_err(|error| {
                postgres_failure(error, || {
                    format!("capture db-state assertion for {case_name}")
                })
            })?;
        let mut stream = std::pin::pin!(stream);
        if stream
            .try_next()
            .await
            .map_err(|error| {
                postgres_failure(error, || {
                    format!("capture db-state assertion for {case_name}")
                })
            })?
            .is_some()
        {
            return Err(local_failure(DbStateCaptureFailureKind::ResultShape));
        }
        return Ok(Vec::new());
    }

    if statement.columns().len() != 1
        || !matches!(statement.columns()[0].type_(), &Type::JSON | &Type::JSONB)
    {
        return Err(local_failure(DbStateCaptureFailureKind::ResultShape));
    }

    let sql = bounded_observation_sql(query, params.len()).map_err(local_failure)?;
    let mut bounded_params = params.to_vec();
    let row_guard = limits
        .max_rows
        .checked_add(1)
        .ok_or_else(|| local_failure(DbStateCaptureFailureKind::ResultShape))?;
    bounded_params.push(row_guard.to_string());
    bounded_params.push(limits.max_json_bytes.to_string());
    let references: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = bounded_params
        .iter()
        .map(|value| value as &(dyn tokio_postgres::types::ToSql + Sync))
        .collect();
    let stream = transaction
        .query_raw(&sql, references)
        .await
        .map_err(|error| {
            bounded_postgres_failure(error, || {
                format!("capture bounded db-state assertion for {case_name}")
            })
        })?;
    let mut stream = std::pin::pin!(stream);
    let mut budget = MaterializationBudget::new(limits);
    let mut rows = Vec::new();

    while let Some(row) = stream.try_next().await.map_err(|error| {
        bounded_postgres_failure(error, || {
            format!("stream bounded db-state assertion for {case_name}")
        })
    })? {
        budget.begin_row().map_err(local_failure)?;
        let row_bytes: Option<i64> = row
            .try_get(1)
            .map_err(|_| local_failure(DbStateCaptureFailureKind::ResultShape))?;
        let row_bytes = row_bytes
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| local_failure(DbStateCaptureFailureKind::ResultShape))?;
        budget.add_json_bytes(row_bytes).map_err(local_failure)?;

        let json_text: Option<String> = row
            .try_get(0)
            .map_err(|_| local_failure(DbStateCaptureFailureKind::ResultShape))?;
        let json_text =
            json_text.ok_or_else(|| local_failure(DbStateCaptureFailureKind::ResultShape))?;
        if json_text.len() != row_bytes {
            return Err(local_failure(DbStateCaptureFailureKind::ResultShape));
        }
        let value = serde_json::from_str(&json_text)
            .map_err(|_| local_failure(DbStateCaptureFailureKind::ResultShape))?;
        rows.push(value);
    }
    Ok(rows)
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
    capture_db_assertions_with_limits(client, case, DbStateCaptureLimits::POC).await
}

/// Capture stored DbState assertions with an explicit finite limit policy.
///
/// Each query is server-time-bounded, fetched through a one-row sentinel limit,
/// and streamed as JSON text. PostgreSQL suppresses any single JSON value larger
/// than the byte limit before it crosses the protocol boundary; the client then
/// applies checked cumulative accounting before parsing or retaining the row.
pub async fn capture_db_assertions_with_limits(
    client: &mut Client,
    case: &TestCase,
    limits: DbStateCaptureLimits,
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
        let transaction = client
            .build_transaction()
            .read_only(true)
            .start()
            .await
            .map_err(|error| {
                postgres_failure(error, || {
                    format!("start db-state observation for {}", case.name)
                })
            })?;
        let query_result =
            capture_observation(&transaction, &case.name, query, &owned, limits).await;
        let rollback_result = transaction.rollback().await;
        let rows = match finish_observation(query_result, rollback_result) {
            Ok(rows) => rows,
            Err(ObservationError::Query(error)) => return Err(error),
            Err(ObservationError::Rollback(error)) => {
                return Err(postgres_failure(error, || {
                    format!("rollback db-state observation for {}", case.name)
                }));
            }
        };
        captures.push(DbCapture {
            query: query.clone(),
            params: params.clone(),
            rows,
        });
    }
    Ok(captures)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_failure_displays_do_not_echo_sensitive_input() {
        let expected = [
            (
                DbStateCaptureFailureKind::ReadOnlyViolation,
                "db-state observation rejected a write",
            ),
            (
                DbStateCaptureFailureKind::TransactionControl,
                "db-state observation rejected transaction control",
            ),
            (
                DbStateCaptureFailureKind::StatementTimeout,
                "db-state observation exceeded its statement timeout",
            ),
            (
                DbStateCaptureFailureKind::RowLimit,
                "db-state observation exceeded its row limit",
            ),
            (
                DbStateCaptureFailureKind::ByteLimit,
                "db-state observation exceeded its JSON byte limit",
            ),
            (
                DbStateCaptureFailureKind::Cancelled,
                "db-state observation was cancelled",
            ),
            (
                DbStateCaptureFailureKind::DependencyUnavailable,
                "db-state observation lost its PostgreSQL dependency",
            ),
            (
                DbStateCaptureFailureKind::ResultShape,
                "db-state observation must return one non-null JSON column",
            ),
        ];
        for (kind, message) in expected {
            let failure = DbStateCaptureFailure::local(kind);
            assert_eq!(kind.message(), message);
            assert_eq!(failure.to_string(), message);
            assert_eq!(
                format!("{failure:?}"),
                format!("DbStateCaptureFailure {{ kind: {kind:?} }}")
            );
        }
    }

    #[test]
    fn transaction_sqlstates_are_classified_intentionally() {
        assert_eq!(
            classify_sqlstate(&SqlState::READ_ONLY_SQL_TRANSACTION, ""),
            Some(DbStateCaptureFailureKind::ReadOnlyViolation)
        );
        assert_eq!(
            classify_sqlstate(&SqlState::ACTIVE_SQL_TRANSACTION, ""),
            Some(DbStateCaptureFailureKind::TransactionControl)
        );
        assert_eq!(
            classify_sqlstate(
                &SqlState::QUERY_CANCELED,
                "canceling statement due to statement timeout"
            ),
            Some(DbStateCaptureFailureKind::StatementTimeout)
        );
        assert_eq!(
            classify_sqlstate(
                &SqlState::QUERY_CANCELED,
                "canceling statement due to user request"
            ),
            Some(DbStateCaptureFailureKind::Cancelled)
        );
        assert_eq!(
            classify_sqlstate(&SqlState::ADMIN_SHUTDOWN, ""),
            Some(DbStateCaptureFailureKind::DependencyUnavailable)
        );
        assert_eq!(classify_sqlstate(&SqlState::SYNTAX_ERROR, ""), None);
    }

    #[test]
    fn bounding_wrapper_identifies_only_its_nested_write_rejection() {
        assert!(is_bounded_wrapper_write_rejection(
            &SqlState::FEATURE_NOT_SUPPORTED,
            NESTED_DATA_MODIFYING_CTE_MESSAGE
        ));
        assert!(!is_bounded_wrapper_write_rejection(
            &SqlState::FEATURE_NOT_SUPPORTED,
            "another unsupported feature"
        ));
        assert!(!is_bounded_wrapper_write_rejection(
            &SqlState::SYNTAX_ERROR,
            NESTED_DATA_MODIFYING_CTE_MESSAGE
        ));
    }

    #[test]
    fn limits_are_finite_non_zero_and_document_the_poc_policy() {
        assert_eq!(
            DbStateCaptureLimits::new(Duration::from_secs(5), 256, 1024 * 1024),
            Some(DbStateCaptureLimits::POC)
        );
        assert_eq!(
            DbStateCaptureLimits::POC.statement_timeout(),
            Duration::from_secs(5)
        );
        assert_eq!(DbStateCaptureLimits::POC.max_rows(), 256);
        assert_eq!(DbStateCaptureLimits::POC.max_json_bytes(), 1024 * 1024);
        assert!(DbStateCaptureLimits::new(Duration::ZERO, 1, 1).is_none());
        assert!(DbStateCaptureLimits::new(Duration::from_millis(1), 0, 1).is_none());
        assert!(DbStateCaptureLimits::new(Duration::from_millis(1), 1, 0).is_none());
    }

    #[test]
    fn statement_timeout_setup_is_transaction_local_and_bound() {
        assert_eq!(
            SET_STATEMENT_TIMEOUT_SQL,
            "SELECT set_config('statement_timeout', $1, true)"
        );
    }

    #[test]
    fn bounded_query_fetches_exactly_one_sentinel_and_guards_each_value() {
        let sql = bounded_observation_sql("SELECT $1::text::jsonb;", 1).unwrap();
        assert!(sql.contains("WITH wamn_db_state(wamn_value) AS (\nSELECT $1::text::jsonb\n)"));
        assert!(sql.contains("LIMIT $2::text::bigint"));
        assert!(sql.contains("octet_length(json_text)::bigint <= $3::text::bigint"));
        assert!(sql.contains("THEN json_text"));
    }

    #[test]
    fn row_limit_allows_the_boundary_then_rejects_the_sentinel() {
        let limits = DbStateCaptureLimits::new(Duration::from_millis(1), 2, 10).unwrap();
        let mut budget = MaterializationBudget::new(limits);
        assert_eq!(budget.begin_row(), Ok(()));
        assert_eq!(budget.begin_row(), Ok(()));
        assert_eq!(budget.begin_row(), Err(DbStateCaptureFailureKind::RowLimit));
    }

    #[test]
    fn byte_limit_allows_the_boundary_then_rejects_the_next_row() {
        let limits = DbStateCaptureLimits::new(Duration::from_millis(1), 2, 10).unwrap();
        let mut budget = MaterializationBudget::new(limits);
        assert_eq!(budget.add_json_bytes(10), Ok(()));
        assert_eq!(
            budget.add_json_bytes(1),
            Err(DbStateCaptureFailureKind::ByteLimit)
        );

        budget.json_bytes = usize::MAX;
        assert_eq!(
            budget.add_json_bytes(1),
            Err(DbStateCaptureFailureKind::ByteLimit)
        );
    }

    #[test]
    fn query_rejection_takes_precedence_over_rollback_failure() {
        let result = finish_observation::<(), _, _>(Err("query"), Err("rollback"));
        assert!(matches!(result, Err(ObservationError::Query("query"))));
    }
}
