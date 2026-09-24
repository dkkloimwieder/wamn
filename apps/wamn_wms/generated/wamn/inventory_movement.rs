// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct InventoryMovementRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub created_by: wamn_postgres_statements::Uuid,
    pub from_location_id: Option<wamn_postgres_statements::Uuid>,
    pub id: wamn_postgres_statements::Uuid,
    pub idempotency_key: String,
    pub kind: String,
    pub occurred_at: wamn_postgres_statements::TimestampTz,
    pub pallet_id: wamn_postgres_statements::Uuid,
    pub product_id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
    pub reason_code: Option<String>,
    pub to_location_id: Option<wamn_postgres_statements::Uuid>,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:06e6b0d81cbaad327ad71bb4b39aa443650f4f021a6927773b7a302c6a21fd78";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:75b2fce7ff0d02c9d272b28220d29d10a50f9199c35075531368719885dc3bde";

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
            idempotency_key: row.decode("idempotency_key")?,
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
) -> Result<Vec<InventoryMovementRow>, wamn_postgres_statements::StatementError> {
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
        Ok(InventoryMovementRow {
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            from_location_id: row.decode("from_location_id")?,
            id: row.decode("id")?,
            idempotency_key: row.decode("idempotency_key")?,
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
