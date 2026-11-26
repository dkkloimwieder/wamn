//! Check projected references under the deletion transaction.

use crate::administration::{ConnectionError, refuse};
use sqlx::Row as _;

use super::administration::database_error;

/// Refuse deleting an external system that this project database still references.
pub(super) async fn refuse_external_system_references(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: uuid::Uuid,
) -> Result<(), ConnectionError> {
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
            "SELECT EXISTS(SELECT FROM {relation} WHERE {column}=$1)"
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
    Ok(())
}
