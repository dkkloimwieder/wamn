// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct WidgetTagRow {
    pub edit_version: i64,
    pub id: wamn_postgres_statements::Uuid,
    pub label: String,
}

#[derive(Debug)]
pub struct WidgetTagUpdateRow {
    pub outcome: Option<String>,
    pub observed_edit_version: Option<i64>,
    pub edit_version: Option<i64>,
    pub id: Option<wamn_postgres_statements::Uuid>,
    pub label: Option<String>,
}

pub(crate) const UPDATE_DIGEST: &str =
    "sha256:002f42391ef131902bcb20cbb5dffa67b8fc6f5fd62bed0bdd91725e2b9ffa2e";

pub(crate) const UPDATE_UNIQUE_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];

pub(crate) async fn update(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
    expected_edit_version: i64,
    label_present: bool,
    label_value: Option<String>,
) -> Result<WidgetTagUpdateRow, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            UPDATE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(expected_edit_version),
                wamn_postgres_statements::into_sql_value(label_present),
                wamn_postgres_statements::into_sql_value(label_value),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(UPDATE_DIGEST, rows, |row| {
        Ok(WidgetTagUpdateRow {
            outcome: row.decode("outcome")?,
            observed_edit_version: row.decode("observed_edit_version")?,
            edit_version: row.decode("edit_version")?,
            id: row.decode("id")?,
            label: row.decode("label")?,
        })
    })
}
