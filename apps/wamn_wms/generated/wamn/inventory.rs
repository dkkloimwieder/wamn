// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct InventoryRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub disposition: String,
    pub id: wamn_postgres_statements::Uuid,
    pub lifecycle: String,
    pub location_id: wamn_postgres_statements::Uuid,
    pub packaging_id: wamn_postgres_statements::Uuid,
    pub product_id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
    pub row_version: i32,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:294b707df48d9141d0fa733634669ef8997be001f46926922097037ccd649bc5";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:02b6b2e398218b77e47568a8236481536250e643de940eef072ca65fc553bacb";

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<InventoryRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(InventoryRow {
            created_at: row.decode("created_at")?,
            disposition: row.decode("disposition")?,
            id: row.decode("id")?,
            lifecycle: row.decode("lifecycle")?,
            location_id: row.decode("location_id")?,
            packaging_id: row.decode("packaging_id")?,
            product_id: row.decode("product_id")?,
            quantity: row.decode("quantity")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<InventoryRow>, wamn_postgres_statements::StatementError> {
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
        Ok(InventoryRow {
            created_at: row.decode("created_at")?,
            disposition: row.decode("disposition")?,
            id: row.decode("id")?,
            lifecycle: row.decode("lifecycle")?,
            location_id: row.decode("location_id")?,
            packaging_id: row.decode("packaging_id")?,
            product_id: row.decode("product_id")?,
            quantity: row.decode("quantity")?,
            row_version: row.decode("row_version")?,
        })
    })
}
