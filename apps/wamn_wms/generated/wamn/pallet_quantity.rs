// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct PalletQuantityRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub pallet_id: wamn_postgres_statements::Uuid,
    pub product_id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
    pub status: String,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:0ecca43ac6e7a244cc0fbb15f58eed6e1bb31116d97dacb7c2cec0767f54c24d";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:889ddeb5097109eee5cef9db5cafa43fff62d4fe24feaed2a2204eff8cffd05a";

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<PalletQuantityRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(PalletQuantityRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            pallet_id: row.decode("pallet_id")?,
            product_id: row.decode("product_id")?,
            quantity: row.decode("quantity")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<PalletQuantityRow>,
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
                Ok(PalletQuantityRow {
                    created_at: row.decode("created_at")?,
                    id: row.decode("id")?,
                    pallet_id: row.decode("pallet_id")?,
                    product_id: row.decode("product_id")?,
                    quantity: row.decode("quantity")?,
                    status: row.decode("status")?,
                })
            },
        )
        .await
}
