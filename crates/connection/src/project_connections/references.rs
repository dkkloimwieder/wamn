//! Check projected references under the deletion transaction.

use crate::administration::{ConnectionError, refuse};
use sqlx::Row as _;

use super::administration::database_error;

fn projection_relation(group: &str) -> Result<&'static str, ConnectionError> {
    match group {
        "connection" => Ok("platform.connection"),
        "external_system" => Ok("platform.external_system"),
        _ => Err(refuse("invalid_input", "operation")),
    }
}

/// Refuse deleting a row that this environment database still references.
pub(super) async fn refuse_environment_references(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    group: &str,
    id: &str,
) -> Result<(), ConnectionError> {
    let projection = projection_relation(group)?;
    // Foreign-key inserts wait here until deletion commits or rolls back.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT id FROM {projection} WHERE id=$1::text::uuid FOR UPDATE"
    )))
    .bind(id)
    .fetch_all(&mut **tx)
    .await
    .map_err(database_error)?;
    if group == "connection" {
        for (field, sql) in [(
            "file",
            "SELECT EXISTS(SELECT FROM platform.file WHERE connection_id=$1::text::uuid)",
        )] {
            if sqlx::query(sqlx::AssertSqlSafe((sql).to_string()))
                .bind(id)
                .fetch_one(&mut **tx)
                .await
                .map_err(database_error)?
                .get::<bool, _>(0)
            {
                return Err(refuse("referenced", field));
            }
        }
    } else if group == "external_system" {
        // A foreign key identifies every record source, including application tables.
        for row in sqlx::query(
            "SELECT quote_ident(n.nspname)||'.'||quote_ident(t.relname), quote_ident(a.attname) \
             FROM pg_constraint c JOIN pg_class t ON t.oid=c.conrelid \
             JOIN pg_namespace n ON n.oid=t.relnamespace \
             JOIN pg_attribute a ON a.attrelid=c.conrelid AND a.attnum=c.conkey[1] \
             WHERE c.contype='f' AND c.confrelid='platform.external_system'::regclass \
               AND cardinality(c.conkey)=1",
        )
        .fetch_all(&mut **tx)
        .await
        .map_err(database_error)?
        {
            let relation: String = row.get(0);
            let column: String = row.get(1);
            if sqlx::query(sqlx::AssertSqlSafe(format!(
                "SELECT EXISTS(SELECT FROM {relation} WHERE {column}=$1::text::uuid)"
            )))
            .bind(id)
            .fetch_one(&mut **tx)
            .await
            .map_err(database_error)?
            .get::<bool, _>(0)
            {
                return Err(refuse("referenced", "record"));
            }
        }
    }
    Ok(())
}
