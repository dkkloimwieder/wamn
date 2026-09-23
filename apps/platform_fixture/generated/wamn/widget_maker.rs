// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct WidgetMakerRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub name: String,
}

pub(crate) const QUERY_0_DIGEST: &str =
    "sha256:e61b9f01a671b9b6b2a2b37a863ad7b246b42aad3e2fcb0ec055e70fd99e23d3";
pub(crate) const QUERY_1_DIGEST: &str =
    "sha256:a3390cf3fe289e153cd85e4334304105b93568f5590687bafbd4a55a25fa013b";

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    name_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<WidgetMakerRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            QUERY_0_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(name_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_0_DIGEST, rows, |row| {
        Ok(WidgetMakerRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            name: row.decode("name")?,
        })
    })
}

pub(crate) async fn query_created_at_descending(
    connection: &mut Connection,
    name_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<WidgetMakerRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            QUERY_1_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(name_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_1_DIGEST, rows, |row| {
        Ok(WidgetMakerRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            name: row.decode("name")?,
        })
    })
}
