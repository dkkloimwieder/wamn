use postgres_protocol::escape;
use sqlx::{Row as _, Statement as _};
use std::fmt::Write as _;

use tracing::Instrument as _;

use wamn_event::Causation;
use wamn_identity_protocol::{AuthorityClass, PrincipalId};
use wamn_package::canonical_name::CanonicalName;
use wamn_registry::ApplicationName;

use wamn_application::database::SqlError;
use wamn_application::session_identity::{SessionIdentity, validate_sql_identity};

use crate::transaction_identity_sql;

use super::values::map_pg_error;

pub(super) fn causation_sql(run: &Causation) -> String {
    let json = serde_json::to_string(run).expect("Causation serializes to JSON");
    format!(
        "SELECT pg_logical_emit_message(true, 'wamn.causation', {}::text);",
        escape::escape_literal(&json)
    )
}

pub(super) async fn finish_transaction(
    conn: &mut sqlx::PgConnection,
    verb: &str,
) -> Result<(), sqlx::Error> {
    if verb == "COMMIT" {
        let statement = sqlx::Executor::prepare(
            &mut *conn,
            sqlx::SqlSafeStr::into_sql_str(sqlx::AssertSqlSafe((verb).to_string())),
        )
        .await?;
        sqlx::query(statement.sql().clone())
            .execute(&mut *conn)
            .await
            .map(|result| result.rows_affected())
            .map(drop)
    } else {
        sqlx::raw_sql(sqlx::AssertSqlSafe((verb).to_string()))
            .execute(&mut *conn)
            .await
            .map(|_| ())
    }
}

/// Open a host write transaction before inserting its caller.
const BEGIN_READ_COMMITTED: &str = "BEGIN ISOLATION LEVEL READ COMMITTED";

/// Open the snapshot before inserting its caller and switching to READ ONLY.
const BEGIN_READ_SNAPSHOT: &str = "BEGIN ISOLATION LEVEL REPEATABLE READ";

/// Caller identity and setup sent in one simple-protocol batch.
#[derive(Debug)]
pub(super) struct BoundIdentity {
    batch: String,
    class: AuthorityClass,
}

impl BoundIdentity {
    pub(super) async fn bind(&self, conn: &mut sqlx::PgConnection) -> Result<String, SqlError> {
        async {
            // Simple protocol permits BEGIN and setup in one round trip. Every
            // value is escaped with PostgreSQL's literal rules before batching.
            let rows = sqlx::raw_sql(sqlx::AssertSqlSafe(self.batch.as_str()))
                .fetch_all(conn)
                .await
                .map_err(|error| map_pg_error(&error))?;
            rows.first()
                .ok_or(sqlx::Error::RowNotFound)
                .and_then(|row| row.try_get(0))
                .map_err(|error| map_pg_error(&error))
        }
        .instrument(tracing::info_span!(
            "wamn.postgres.bind_identity",
            wamn.authority_class = self.class.as_str()
        ))
        .await
    }
}

#[derive(Default)]
pub(super) struct TransactionIdentity<'a> {
    pub(super) schema: Option<&'a str>,
    pub(super) runner: Option<&'a str>,
    pub(super) role: Option<&'a str>,
    pub(super) principal: Option<&'a PrincipalId>,
    pub(super) operation: Option<&'a CanonicalName>,
    pub(super) application: Option<&'a ApplicationName>,
}

impl<'a> From<&'a SessionIdentity> for TransactionIdentity<'a> {
    fn from(identity: &'a SessionIdentity) -> Self {
        Self {
            schema: identity.schema.as_deref(),
            runner: identity.runner.as_deref(),
            role: identity.role.as_deref(),
            principal: identity.user_id.as_ref(),
            operation: identity.operation.as_ref(),
            application: identity.application.as_ref(),
        }
    }
}

pub(super) fn prepare_identity(
    class: AuthorityClass,
    read: bool,
    identity: &TransactionIdentity<'_>,
    statement_timeout_ms: u32,
) -> Result<BoundIdentity, SqlError> {
    let TransactionIdentity {
        schema,
        runner,
        role,
        principal,
        operation,
        application,
    } = *identity;
    // The project selected this connection credential. Refuse malformed values
    // before opening its transaction.
    validate_sql_identity(schema, runner, role)?;
    let principal = principal
        .ok_or_else(|| SqlError::QueryError(("55000".to_owned(), "actor-required".to_owned())))?;
    // The isolation level is stated, not inherited from a session default:
    // the write log's retry reads the identity that its insert waited on, and
    // only a fresh READ COMMITTED snapshot sees it
    // (docs/architecture/execution.md#native-dispatch). A host read
    // transaction (`read`) reads one snapshot.
    let begin = if read {
        BEGIN_READ_SNAPSHOT
    } else {
        BEGIN_READ_COMMITTED
    };
    let bind = transaction_identity_sql(
        principal,
        role.unwrap_or_default(),
        application.map_or("", ApplicationName::as_str),
        operation.map(CanonicalName::as_str),
        runner,
    );
    let schema = schema.map(escape::escape_literal);
    let mut batch = format!("{begin}; {bind} RETURNING pg_current_xact_id()::text;");
    if read {
        batch.push_str("SET TRANSACTION READ ONLY;");
    }
    write!(
        batch,
        "SELECT set_config('statement_timeout', '{statement_timeout_ms}', true), \
             set_config('search_path', COALESCE({}, current_setting('search_path')), true);",
        schema.as_deref().unwrap_or("NULL"),
    )
    .expect("writing transaction setup to String cannot fail");
    Ok(BoundIdentity { batch, class })
}
