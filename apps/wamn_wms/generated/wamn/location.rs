// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct LocationRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub description: Option<String>,
    pub id: wamn_postgres_statements::Uuid,
    pub location_code: String,
    pub row_version: i32,
}

#[derive(Debug)]
pub struct LocationUpdateRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i32>,
    pub created_at: Option<wamn_postgres_statements::TimestampTz>,
    pub description: Option<String>,
    pub id: Option<wamn_postgres_statements::Uuid>,
    pub location_code: Option<String>,
    pub row_version: Option<i32>,
}

pub(crate) const CREATE_DIGEST: &str =
    "sha256:8a81bcb9ba0fb30dfed56045f8c87bae44afa854ff3fd28173cb3b483c8d46c6";
pub(crate) const GET_DIGEST: &str =
    "sha256:a18917ca4ca0baf08491f88207b738b73caeecfd2c128e731268183be0f7d9bf";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:c42e750623ec91f62f6b20349fd1c4eca253ea7a14224790b72f7c05b6fb44d7";
pub(crate) const UPDATE_DIGEST: &str =
    "sha256:42389592831297f5056e3d920ac93598fe80e144d5d8c8111c21ae72ab5c7003";

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
            description: row.decode("description")?,
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
                    description: row.decode("description")?,
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
            description: row.decode("description")?,
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
            description: row.decode("description")?,
            id: row.decode("id")?,
            location_code: row.decode("location_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}
