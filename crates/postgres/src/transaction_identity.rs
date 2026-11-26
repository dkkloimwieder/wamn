//! Caller identity scoped to a PostgreSQL transaction.

use std::fmt::Write as _;

use postgres_protocol::escape;
use postgres_protocol::escape::escape_literal;
use sqlx::{PgConnection, Row as _};
use tracing::Instrument as _;
use wamn_app::database::SqlError;
use wamn_app::session_identity::{SessionIdentity, validate_sql_identity};
use wamn_identity_protocol::AuthorityClass;
use wamn_package::canonical_name::CanonicalName;
use wamn_principal::PrincipalId;
use wamn_registry::AppName;

use crate::map_pg_error;

/// Bind one caller to the open transaction before running protected SQL.
pub async fn bind_transaction_identity(
    connection: &mut PgConnection,
    principal: &PrincipalId,
    role: &str,
    app: &str,
    operation: Option<&str>,
    runner: Option<&str>,
) -> Result<(), sqlx::Error> {
    let statement = transaction_identity_sql(principal, role, app, operation, runner);
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
        .execute(connection)
        .await?;
    Ok(())
}

/// Render the binding statement for a simple-protocol transaction batch.
pub fn transaction_identity_sql(
    principal: &PrincipalId,
    role: &str,
    app: &str,
    operation: Option<&str>,
    runner: Option<&str>,
) -> String {
    let role = escape_literal(role);
    let app = escape_literal(app);
    let operation = operation.map(escape_literal);
    let runner = runner.map(escape_literal);
    format!(
        "INSERT INTO platform.tx_identity (principal_id, role, app, operation, runner) \
         VALUES ('{principal}'::uuid, {role}, {app}, {}, {})",
        operation.as_deref().unwrap_or("NULL"),
        runner.as_deref().unwrap_or("NULL"),
    )
}

/// Open a host write transaction before inserting its caller.
const BEGIN_READ_COMMITTED: &str = "BEGIN ISOLATION LEVEL READ COMMITTED";

/// Open the snapshot before inserting its caller and switching to READ ONLY.
const BEGIN_READ_SNAPSHOT: &str = "BEGIN ISOLATION LEVEL REPEATABLE READ";

/// Caller identity and setup sent in one simple-protocol batch.
#[derive(Debug)]
pub struct BoundIdentity {
    batch: String,
    class: AuthorityClass,
}

impl BoundIdentity {
    /// Start the transaction, bind the caller, and return its transaction ID.
    pub async fn bind(&self, conn: &mut sqlx::PgConnection) -> Result<String, SqlError> {
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

/// Caller identity and schema for one transaction.
#[derive(Debug, Default)]
pub struct TransactionIdentity<'a> {
    pub schema: Option<&'a str>,
    pub runner: Option<&'a str>,
    pub role: Option<&'a str>,
    pub principal: Option<&'a PrincipalId>,
    pub operation: Option<&'a CanonicalName>,
    pub app: Option<&'a AppName>,
}

impl<'a> From<&'a SessionIdentity> for TransactionIdentity<'a> {
    fn from(identity: &'a SessionIdentity) -> Self {
        Self {
            schema: identity.schema.as_deref(),
            runner: identity.runner.as_deref(),
            role: identity.role.as_deref(),
            principal: identity.user_id.as_ref(),
            operation: identity.operation.as_ref(),
            app: identity.app.as_ref(),
        }
    }
}

/// Validate the caller and prepare the transaction setup batch.
pub fn prepare_identity(
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
        app,
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
        app.map_or("", AppName::as_str),
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
