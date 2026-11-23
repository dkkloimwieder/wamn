use postgres_protocol::escape;
use sqlx::{Row as _, Statement as _};
use std::fmt::Write as _;

use tracing::Instrument as _;

use wamn_event::Causation;
use wamn_identity_protocol::AuthorityClass;

use crate::PgError;
use crate::scope_registry::{valid_runner, valid_schema};
use wamn_postgres::transaction_identity_sql;
use super::types::map_pg_error;

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
    pub(super) async fn bind(&self, conn: &mut sqlx::PgConnection) -> Result<String, PgError> {
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

/// Begin a transaction and bind its caller before any application SQL.
#[expect(
    clippy::too_many_arguments,
    reason = "every identity this transaction binds is an independently trusted input"
)]
pub(super) async fn begin_with_identity(
    conn: &mut sqlx::PgConnection,
    class: AuthorityClass,
    read: bool,
    schema: Option<&str>,
    runner: Option<&str>,
    role: Option<&str>,
    user_id: Option<&wamn_identity_protocol::PrincipalId>,
    operation: Option<&wamn_package::canonical_name::CanonicalName>,
    application: Option<&wamn_registry::ApplicationName>,
    run: Option<&Causation>,
    statement_timeout_ms: u32,
) -> Result<String, PgError> {
    prepare_identity(
        class,
        read,
        schema,
        runner,
        role,
        user_id,
        operation,
        application,
        run,
        statement_timeout_ms,
    )?
    .bind(conn)
    .await
}

#[expect(
    clippy::too_many_arguments,
    reason = "every identity this transaction binds is an independently trusted input"
)]
pub(super) fn prepare_identity(
    class: AuthorityClass,
    read: bool,
    schema: Option<&str>,
    runner: Option<&str>,
    role: Option<&str>,
    user_id: Option<&wamn_identity_protocol::PrincipalId>,
    operation: Option<&wamn_package::canonical_name::CanonicalName>,
    application: Option<&wamn_registry::ApplicationName>,
    run: Option<&Causation>,
    statement_timeout_ms: u32,
) -> Result<BoundIdentity, PgError> {
    // The project selected this connection credential. Refuse malformed values
    // before opening its transaction.
    validate_session_identity(schema, runner, role, operation)?;
    let principal = user_id
        .ok_or_else(|| PgError::QueryError(("55000".to_owned(), "actor-required".to_owned())))?;
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
        application.map_or("", wamn_registry::ApplicationName::as_str),
        operation.map(wamn_package::canonical_name::CanonicalName::as_str),
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
    if let Some(run) = run.filter(|_| !read) {
        batch.push_str(&causation_sql(run));
    }
    Ok(BoundIdentity { batch, class })
}

/// Reject a malformed session identity before it is bound (R16). Since R2 these
/// validators are not the injection boundary: identity values use PostgreSQL
/// literal escaping in the begin batch, so a `'`/`;`/`--` value is inert data, but a
/// malformed identity still fails closed: they define what a *legal* id is (and
/// the no-hyphen `valid_schema` rule still matters where a schema name is quoted
/// into DDL elsewhere).
pub(super) fn validate_session_identity(
    schema: Option<&str>,
    runner: Option<&str>,
    role: Option<&str>,
    _operation: Option<&wamn_package::canonical_name::CanonicalName>,
) -> Result<(), PgError> {
    if let Some(schema) = schema
        && !valid_schema(schema)
    {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid search_path schema".to_string(),
        )));
    }
    if let Some(runner) = runner
        && !valid_runner(runner)
    {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid runner owner".to_string(),
        )));
    }
    if let Some(role) = role
        && !role
            .parse::<wamn_identity_protocol::RoleName>()
            .is_ok_and(|parsed| parsed.as_str() == role)
    {
        return Err(PgError::QueryError((
            "WAMN0".to_string(),
            "invalid caller role".to_string(),
        )));
    }
    Ok(())
}
