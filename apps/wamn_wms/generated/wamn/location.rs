// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct LocationRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub location_code: String,
    pub row_version: i32,
}

#[derive(Debug)]
pub struct LocationUpdateRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i32>,
    pub created_at: Option<wamn_postgres_statements::TimestampTz>,
    pub id: Option<wamn_postgres_statements::Uuid>,
    pub location_code: Option<String>,
    pub row_version: Option<i32>,
}

pub(crate) const CREATE_DIGEST: &str =
    "sha256:e8c3d64ccf22069080b74ea76953975d8c8e4e71d6f1eee000967a1bb27aa058";
pub(crate) const GET_DIGEST: &str =
    "sha256:61ac14096e05333a1144902bc75008c04282a56c79f864c6b70c0bdf6d4cfb43";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:ff1b26713e83462dd1f81c48250843c65e88c845b0b8b64042a8eba759229497";
pub(crate) const UPDATE_DIGEST: &str =
    "sha256:44379e3dce960c2cb46c2e7a359305f33bc830a1ae3b91c56a9382f06bcc4b07";

pub(crate) const CREATE_UNIQUE_CONSTRAINTS: &[&str] =
    &["location_id_pkey", "location_location_code_key"];
pub(crate) const CREATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const CREATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const CREATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_UNIQUE_CONSTRAINTS: &[&str] = &["location_location_code_key"];
pub(crate) const UPDATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<LocationRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(LocationRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_code: row.decode("location_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    location_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<LocationRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(location_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(LocationRow {
                    created_at: row.decode("created_at")?,
                    id: row.decode("id")?,
                    location_code: row.decode("location_code")?,
                    row_version: row.decode("row_version")?,
                })
            },
        )
        .await
}

pub(crate) async fn create(
    transaction: &mut wamn_postgres_statements::Transaction,
    location_code: String,
) -> Result<LocationRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            CREATE_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(location_code)],
        )
        .await?;
    wamn_postgres_statements::decode_one(CREATE_DIGEST, rows, |row| {
        Ok(LocationRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_code: row.decode("location_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn update(
    transaction: &mut wamn_postgres_statements::Transaction,
    id: wamn_postgres_statements::Uuid,
    expected_row_version: i32,
    location_code_present: bool,
    location_code_value: Option<String>,
) -> Result<LocationUpdateRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            UPDATE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(expected_row_version),
                wamn_postgres_statements::into_sql_value(location_code_present),
                wamn_postgres_statements::into_sql_value(location_code_value),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(UPDATE_DIGEST, rows, |row| {
        Ok(LocationUpdateRow {
            outcome: row.decode("outcome")?,
            observed_row_version: row.decode("observed_row_version")?,
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_code: row.decode("location_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}
