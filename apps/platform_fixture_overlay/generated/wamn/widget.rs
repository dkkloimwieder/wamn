// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct WidgetRow {
    pub code: String,
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub edit_version: i64,
    pub id: wamn_postgres_statements::Uuid,
    pub maker_id: Option<wamn_postgres_statements::Uuid>,
    pub note: Option<String>,
    pub overlay_note: Option<String>,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:8e9feac44d9c26a7f07f8ccd5c0460ab82f40ca7f920409ea9becccfdcc3026c";

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<WidgetRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(WidgetRow {
            code: row.decode("code")?,
            created_at: row.decode("created_at")?,
            edit_version: row.decode("edit_version")?,
            id: row.decode("id")?,
            maker_id: row.decode("maker_id")?,
            note: row.decode("note")?,
            overlay_note: row.decode("overlay_note")?,
        })
    })
}
