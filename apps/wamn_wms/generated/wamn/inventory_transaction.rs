// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct InventoryTransactionRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub from_disposition: Option<String>,
    pub from_inventory_id: wamn_postgres_statements::Uuid,
    pub from_lifecycle: Option<String>,
    pub from_location_id: Option<wamn_postgres_statements::Uuid>,
    pub from_packaging_id: Option<wamn_postgres_statements::Uuid>,
    pub from_product_id: Option<wamn_postgres_statements::Uuid>,
    pub from_quantity: wamn_postgres_statements::Numeric,
    pub id: wamn_postgres_statements::Uuid,
    pub inventory_id: wamn_postgres_statements::Uuid,
    pub occurred_at: wamn_postgres_statements::TimestampTz,
    pub operation_id: wamn_postgres_statements::Uuid,
    pub reason: Option<String>,
    pub to_disposition: String,
    pub to_inventory_id: wamn_postgres_statements::Uuid,
    pub to_lifecycle: String,
    pub to_location_id: wamn_postgres_statements::Uuid,
    pub to_packaging_id: wamn_postgres_statements::Uuid,
    pub to_product_id: wamn_postgres_statements::Uuid,
    pub to_quantity: wamn_postgres_statements::Numeric,
    pub r#type: String,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:415ff10efad420f62823d9ed298308bb70ac86049ebe4ef47242cb36f64ed79f";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:08baa3778e908a88f524e78c72319c3db8b9416299303db56d8d163337e03727";

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<InventoryTransactionRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(InventoryTransactionRow {
            created_at: row.decode("created_at")?,
            from_disposition: row.decode("from_disposition")?,
            from_inventory_id: row.decode("from_inventory_id")?,
            from_lifecycle: row.decode("from_lifecycle")?,
            from_location_id: row.decode("from_location_id")?,
            from_packaging_id: row.decode("from_packaging_id")?,
            from_product_id: row.decode("from_product_id")?,
            from_quantity: row.decode("from_quantity")?,
            id: row.decode("id")?,
            inventory_id: row.decode("inventory_id")?,
            occurred_at: row.decode("occurred_at")?,
            operation_id: row.decode("operation_id")?,
            reason: row.decode("reason")?,
            to_disposition: row.decode("to_disposition")?,
            to_inventory_id: row.decode("to_inventory_id")?,
            to_lifecycle: row.decode("to_lifecycle")?,
            to_location_id: row.decode("to_location_id")?,
            to_packaging_id: row.decode("to_packaging_id")?,
            to_product_id: row.decode("to_product_id")?,
            to_quantity: row.decode("to_quantity")?,
            r#type: row.decode("type")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<InventoryTransactionRow>, wamn_postgres_statements::StatementError> {
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
        Ok(InventoryTransactionRow {
            created_at: row.decode("created_at")?,
            from_disposition: row.decode("from_disposition")?,
            from_inventory_id: row.decode("from_inventory_id")?,
            from_lifecycle: row.decode("from_lifecycle")?,
            from_location_id: row.decode("from_location_id")?,
            from_packaging_id: row.decode("from_packaging_id")?,
            from_product_id: row.decode("from_product_id")?,
            from_quantity: row.decode("from_quantity")?,
            id: row.decode("id")?,
            inventory_id: row.decode("inventory_id")?,
            occurred_at: row.decode("occurred_at")?,
            operation_id: row.decode("operation_id")?,
            reason: row.decode("reason")?,
            to_disposition: row.decode("to_disposition")?,
            to_inventory_id: row.decode("to_inventory_id")?,
            to_lifecycle: row.decode("to_lifecycle")?,
            to_location_id: row.decode("to_location_id")?,
            to_packaging_id: row.decode("to_packaging_id")?,
            to_product_id: row.decode("to_product_id")?,
            to_quantity: row.decode("to_quantity")?,
            r#type: row.decode("type")?,
        })
    })
}
