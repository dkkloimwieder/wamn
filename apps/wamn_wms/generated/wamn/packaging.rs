// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct PackagingRow {
    pub code: String,
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub lifecycle: String,
    pub location_id: wamn_postgres_statements::Uuid,
    pub row_version: i32,
    pub r#type: String,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:03921b1e84a79446775a7dc21e2a4ea3b5b3bd9ac9e65b0524f494e125a703e4";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:2680a7cba380c3e5cc937630898ca3f2f44b8cbeb9b7ab143fd2fab07dee9b3e";

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<PackagingRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(PackagingRow {
            code: row.decode("code")?,
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            lifecycle: row.decode("lifecycle")?,
            location_id: row.decode("location_id")?,
            row_version: row.decode("row_version")?,
            r#type: row.decode("type")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<PackagingRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            QUERY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_DIGEST, rows, |row| {
        Ok(PackagingRow {
            code: row.decode("code")?,
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            lifecycle: row.decode("lifecycle")?,
            location_id: row.decode("location_id")?,
            row_version: row.decode("row_version")?,
            r#type: row.decode("type")?,
        })
    })
}
