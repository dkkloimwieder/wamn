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
    "sha256:5340fcb1b850bef90c6d9e99182944c94fbbef695cccae433ad292c90a738d24";
pub(crate) const QUERY_1_DIGEST: &str =
    "sha256:ac357f0d2b905309980db319ccb371753dda7c5e450717549a9723bbe110a421";

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
    created_at_filter: Option<wamn_postgres_statements::Json>,
    search: Option<String>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<WidgetMakerRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_0_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(name_filter),
                wamn_postgres_statements::into_sql_value(created_at_filter),
                wamn_postgres_statements::into_sql_value(search),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(WidgetMakerRow {
                    created_at: row.decode("created_at")?,
                    edit_version: row.decode("edit_version")?,
                    id: row.decode("id")?,
                    name: row.decode("name")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_created_at_descending(
    connection: &mut Connection,
    name_filter: Option<wamn_postgres_statements::Json>,
    created_at_filter: Option<wamn_postgres_statements::Json>,
    search: Option<String>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<WidgetMakerRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_1_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(name_filter),
                wamn_postgres_statements::into_sql_value(created_at_filter),
                wamn_postgres_statements::into_sql_value(search),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(WidgetMakerRow {
                    created_at: row.decode("created_at")?,
                    edit_version: row.decode("edit_version")?,
                    id: row.decode("id")?,
                    name: row.decode("name")?,
                })
            },
        )
        .await
}
