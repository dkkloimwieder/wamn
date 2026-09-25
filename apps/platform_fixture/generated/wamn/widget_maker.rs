// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct WidgetMakerRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub edit_version: i64,
    pub id: wamn_postgres_statements::Uuid,
    pub name: String,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:9213a9b4970ebf8253c515242917e72f1018e879ddac78f989ba5ade11c9d9a7";
pub(crate) const QUERY_0_DIGEST: &str =
    "sha256:4678a09e1be42d9b1e18e9acfca19e9bf4353c5132b37f3537072a623127abe9";
pub(crate) const QUERY_1_DIGEST: &str =
    "sha256:d54912878bf6e51d45b43fdb7f1298baf78763684ae52fd5cc20ef7cb4472b99";

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<WidgetMakerRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(WidgetMakerRow {
            created_at: row.decode("created_at")?,
            edit_version: row.decode("edit_version")?,
            id: row.decode("id")?,
            name: row.decode("name")?,
        })
    })
}

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
            edit_version: row.decode("edit_version")?,
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
            edit_version: row.decode("edit_version")?,
            id: row.decode("id")?,
            name: row.decode("name")?,
        })
    })
}
