//! Caller identity scoped to a PostgreSQL transaction.

use postgres_protocol::escape::escape_literal;
use sqlx::PgConnection;
use wamn_principal::PrincipalId;

/// Bind one caller to the open transaction before running protected SQL.
pub async fn bind_transaction_identity(
    connection: &mut PgConnection,
    principal: &PrincipalId,
    role: &str,
    application: &str,
    operation: Option<&str>,
    runner: Option<&str>,
) -> Result<(), sqlx::Error> {
    let statement = transaction_identity_sql(principal, role, application, operation, runner);
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement))
        .execute(connection)
        .await?;
    Ok(())
}

/// Render the binding statement for a simple-protocol transaction batch.
pub fn transaction_identity_sql(
    principal: &PrincipalId,
    role: &str,
    application: &str,
    operation: Option<&str>,
    runner: Option<&str>,
) -> String {
    let role = escape_literal(role);
    let application = escape_literal(application);
    let operation = operation.map(escape_literal);
    let runner = runner.map(escape_literal);
    format!(
        "INSERT INTO platform.tx_identity (principal_id, role, application, operation, runner) \
         VALUES ('{principal}'::uuid, {role}, {application}, {}, {})",
        operation.as_deref().unwrap_or("NULL"),
        runner.as_deref().unwrap_or("NULL"),
    )
}
