use postgres_protocol::escape;
use sqlx::Statement as _;

use wamn_event::Causation;

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
