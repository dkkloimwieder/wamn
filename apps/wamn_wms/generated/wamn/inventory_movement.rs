// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct InventoryMovementRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub created_by: wamn_postgres_statements::Uuid,
    pub from_location_id: Option<wamn_postgres_statements::Uuid>,
    pub id: wamn_postgres_statements::Uuid,
    pub kind: String,
    pub occurred_at: wamn_postgres_statements::TimestampTz,
    pub pallet_id: wamn_postgres_statements::Uuid,
    pub product_id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
    pub reason_code: Option<String>,
    pub to_location_id: Option<wamn_postgres_statements::Uuid>,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:bf9f3519935253b1063192015303a25f8626eca1769f03bda6e4c3acdf6859f8";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:b2e0988b36ddd211e98c1f94cfa1b034ecad8f95872ba55d948cbc827350aeee";

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<InventoryMovementRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(InventoryMovementRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            from_location_id: row.decode("from_location_id")?,
            id: row.decode("id")?,
            kind: row.decode("kind")?,
            occurred_at: row.decode("occurred_at")?,
            pallet_id: row.decode("pallet_id")?,
            product_id: row.decode("product_id")?,
            quantity: row.decode("quantity")?,
            reason_code: row.decode("reason_code")?,
            to_location_id: row.decode("to_location_id")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<InventoryMovementRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(InventoryMovementRow {
                    created_at: row.decode("created_at")?,
                    created_by: row.decode("created_by")?,
                    from_location_id: row.decode("from_location_id")?,
                    id: row.decode("id")?,
                    kind: row.decode("kind")?,
                    occurred_at: row.decode("occurred_at")?,
                    pallet_id: row.decode("pallet_id")?,
                    product_id: row.decode("product_id")?,
                    quantity: row.decode("quantity")?,
                    reason_code: row.decode("reason_code")?,
                    to_location_id: row.decode("to_location_id")?,
                })
            },
        )
        .await
}
