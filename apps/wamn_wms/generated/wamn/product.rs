// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct ProductRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub product_code: String,
    pub row_version: i32,
}

#[derive(Debug)]
pub struct ProductUpdateRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i32>,
    pub created_at: Option<wamn_postgres_statements::TimestampTz>,
    pub id: Option<wamn_postgres_statements::Uuid>,
    pub product_code: Option<String>,
    pub row_version: Option<i32>,
}

pub(crate) const CREATE_DIGEST: &str =
    "sha256:606f817630770d089bbf58cd641b90207c6e783393a73817839b9e6330b45fcd";
pub(crate) const GET_DIGEST: &str =
    "sha256:93087bc679532d835ef2520dcba91bd0a4df2788792926c814269641944b9157";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:8dad26fccbc8d5c3cd68b98d33add3b5bff807506f75b4a2025fe913c8549261";
pub(crate) const UPDATE_DIGEST: &str =
    "sha256:657b27225661e113aad5fca5ad3401b399a37bdb3707b3d78b172465369ffcad";

pub(crate) const CREATE_UNIQUE_CONSTRAINTS: &[&str] =
    &["product_id_pkey", "product_product_code_key"];
pub(crate) const CREATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const CREATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const CREATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_UNIQUE_CONSTRAINTS: &[&str] = &["product_product_code_key"];
pub(crate) const UPDATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<ProductRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(ProductRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            product_code: row.decode("product_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    product_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<wamn_postgres_statements::RowStream<ProductRow>, wamn_postgres_statements::StatementError>
{
    connection
        .run_stream(
            QUERY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(product_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(ProductRow {
                    created_at: row.decode("created_at")?,
                    id: row.decode("id")?,
                    product_code: row.decode("product_code")?,
                    row_version: row.decode("row_version")?,
                })
            },
        )
        .await
}

pub(crate) async fn create(
    transaction: &mut wamn_postgres_statements::Transaction,
    product_code: String,
) -> Result<ProductRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            CREATE_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(product_code)],
        )
        .await?;
    wamn_postgres_statements::decode_one(CREATE_DIGEST, rows, |row| {
        Ok(ProductRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            product_code: row.decode("product_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn update(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
    expected_row_version: i32,
    product_code_present: bool,
    product_code_value: Option<String>,
) -> Result<ProductUpdateRow, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            UPDATE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(expected_row_version),
                wamn_postgres_statements::into_sql_value(product_code_present),
                wamn_postgres_statements::into_sql_value(product_code_value),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(UPDATE_DIGEST, rows, |row| {
        Ok(ProductUpdateRow {
            outcome: row.decode("outcome")?,
            observed_row_version: row.decode("observed_row_version")?,
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            product_code: row.decode("product_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}
