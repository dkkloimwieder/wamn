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
}

#[derive(Debug)]
pub struct WidgetUpdateRow {
    pub outcome: Option<String>,
    pub observed_edit_version: Option<i64>,
    pub code: Option<String>,
    pub created_at: Option<wamn_postgres_statements::TimestampTz>,
    pub edit_version: Option<i64>,
    pub id: Option<wamn_postgres_statements::Uuid>,
    pub maker_id: Option<wamn_postgres_statements::Uuid>,
    pub note: Option<String>,
}

#[derive(Debug)]
pub struct WidgetDeleteRow {
    pub outcome: Option<String>,
    pub observed_edit_version: Option<i64>,
}

pub(crate) const CREATE_DIGEST: &str =
    "sha256:4beebeaa6a19f314c9a3c993585207896d71982988720af3c7d820c7a4995513";
pub(crate) const DELETE_DIGEST: &str =
    "sha256:bbd65bd9876edc16ad8b39dadf056eed11a3476e39f4ad368a4bd7927565e35f";
pub(crate) const GET_DIGEST: &str =
    "sha256:9033b3a1caa6ee73ba3aa4a5df84824c1aaf806e3e7c90329e2a9a3324e9b9bf";
pub(crate) const QUERY_0_DIGEST: &str =
    "sha256:dcb9f07cc0496bb54721a3bb9c6df6fd15f1614d2e4ebf6f32ac3513a63b19e1";
pub(crate) const QUERY_1_DIGEST: &str =
    "sha256:dfbee8737b4ddca521e79a1d2559d61cd55a3fbe439f0ce46486973844fc0172";
pub(crate) const UPDATE_DIGEST: &str =
    "sha256:a012c26134f98282143795c4c02c39ff6b7db89ef85661c279a275a3125ef540";

pub(crate) const CREATE_UNIQUE_CONSTRAINTS: &[&str] = &["widget_code_key", "widget_id_pkey"];
pub(crate) const CREATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &["widget_maker_id_fkey"];
pub(crate) const CREATE_CHECK_CONSTRAINTS: &[&str] = &["widget_code_check"];
pub(crate) const CREATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_UNIQUE_CONSTRAINTS: &[&str] = &["widget_code_key"];
pub(crate) const UPDATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &["widget_maker_id_fkey"];
pub(crate) const UPDATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];
pub(crate) const DELETE_UNIQUE_CONSTRAINTS: &[&str] = &[];
pub(crate) const DELETE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const DELETE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const DELETE_EXCLUSION_CONSTRAINTS: &[&str] = &[];

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
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    code_filter: Option<wamn_postgres_statements::Json>,
    note_filter: Option<wamn_postgres_statements::Json>,
    maker_id_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<wamn_postgres_statements::RowStream<WidgetRow>, wamn_postgres_statements::StatementError>
{
    connection
        .run_stream(
            QUERY_0_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(code_filter),
                wamn_postgres_statements::into_sql_value(note_filter),
                wamn_postgres_statements::into_sql_value(maker_id_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(WidgetRow {
                    code: row.decode("code")?,
                    created_at: row.decode("created_at")?,
                    edit_version: row.decode("edit_version")?,
                    id: row.decode("id")?,
                    maker_id: row.decode("maker_id")?,
                    note: row.decode("note")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_created_at_descending(
    connection: &mut Connection,
    code_filter: Option<wamn_postgres_statements::Json>,
    note_filter: Option<wamn_postgres_statements::Json>,
    maker_id_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<wamn_postgres_statements::RowStream<WidgetRow>, wamn_postgres_statements::StatementError>
{
    connection
        .run_stream(
            QUERY_1_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(code_filter),
                wamn_postgres_statements::into_sql_value(note_filter),
                wamn_postgres_statements::into_sql_value(maker_id_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(WidgetRow {
                    code: row.decode("code")?,
                    created_at: row.decode("created_at")?,
                    edit_version: row.decode("edit_version")?,
                    id: row.decode("id")?,
                    maker_id: row.decode("maker_id")?,
                    note: row.decode("note")?,
                })
            },
        )
        .await
}

pub(crate) async fn create(
    transaction: &mut wamn_postgres_statements::Transaction,
    code: String,
    maker_id: Option<wamn_postgres_statements::Uuid>,
    note: Option<String>,
) -> Result<WidgetRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            CREATE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(code),
                wamn_postgres_statements::into_sql_value(maker_id),
                wamn_postgres_statements::into_sql_value(note),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(CREATE_DIGEST, rows, |row| {
        Ok(WidgetRow {
            code: row.decode("code")?,
            created_at: row.decode("created_at")?,
            edit_version: row.decode("edit_version")?,
            id: row.decode("id")?,
            maker_id: row.decode("maker_id")?,
            note: row.decode("note")?,
        })
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the parameters are the statement's bind list"
)]
pub(crate) async fn update(
    transaction: &mut wamn_postgres_statements::Transaction,
    id: wamn_postgres_statements::Uuid,
    expected_edit_version: i64,
    code_present: bool,
    code_value: Option<String>,
    maker_id_present: bool,
    maker_id_value: Option<wamn_postgres_statements::Uuid>,
    note_present: bool,
    note_value: Option<String>,
) -> Result<WidgetUpdateRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            UPDATE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(expected_edit_version),
                wamn_postgres_statements::into_sql_value(code_present),
                wamn_postgres_statements::into_sql_value(code_value),
                wamn_postgres_statements::into_sql_value(maker_id_present),
                wamn_postgres_statements::into_sql_value(maker_id_value),
                wamn_postgres_statements::into_sql_value(note_present),
                wamn_postgres_statements::into_sql_value(note_value),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(UPDATE_DIGEST, rows, |row| {
        Ok(WidgetUpdateRow {
            outcome: row.decode("outcome")?,
            observed_edit_version: row.decode("observed_edit_version")?,
            code: row.decode("code")?,
            created_at: row.decode("created_at")?,
            edit_version: row.decode("edit_version")?,
            id: row.decode("id")?,
            maker_id: row.decode("maker_id")?,
            note: row.decode("note")?,
        })
    })
}

pub(crate) async fn delete(
    transaction: &mut wamn_postgres_statements::Transaction,
    id: wamn_postgres_statements::Uuid,
    expected_edit_version: i64,
) -> Result<WidgetDeleteRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            DELETE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(expected_edit_version),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(DELETE_DIGEST, rows, |row| {
        Ok(WidgetDeleteRow {
            outcome: row.decode("outcome")?,
            observed_edit_version: row.decode("observed_edit_version")?,
        })
    })
}
