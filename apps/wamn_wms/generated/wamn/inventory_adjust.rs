// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct InsertMovementRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct LockPalletRow {
    pub location_id: wamn_postgres_statements::Uuid,
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug)]
pub struct SetQuantityRow {
    pub id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub struct TouchPalletRow {
    pub row_version: i32,
    pub status: String,
}

pub(crate) const INSERT_MOVEMENT_DIGEST: &str =
    "sha256:9b38401dc9b845523fcfe61f809519096574a09968dd68bbc1ea579bfa739ea4";
pub(crate) const LOCK_PALLET_DIGEST: &str =
    "sha256:a55bfebbebf5bba9540074165b1e5750116fda67b8ef07439c89469ed1ffece3";
pub(crate) const SET_QUANTITY_DIGEST: &str =
    "sha256:013414bd90ba990f46f429326fbb956c10e709e3a355ee5f91fcea7d82c3ef86";
pub(crate) const TOUCH_PALLET_DIGEST: &str =
    "sha256:1015523c5b8bfcec2e8b84ed44c5b1a20fb39209656c0f78f3f68408850070d9";

pub(crate) async fn insert_movement(
    transaction: &mut Transaction,
    pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    quantity: wamn_postgres_statements::Numeric,
    reason_code: String,
    occurred_at: wamn_postgres_statements::TimestampTz,
) -> Result<InsertMovementRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            INSERT_MOVEMENT_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(pallet_id),
                wamn_postgres_statements::into_sql_value(product_id),
                wamn_postgres_statements::into_sql_value(quantity),
                wamn_postgres_statements::into_sql_value(reason_code),
                wamn_postgres_statements::into_sql_value(occurred_at),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(INSERT_MOVEMENT_DIGEST, rows, |row| {
        Ok(InsertMovementRow {
            id: row.decode("id")?,
        })
    })
}

pub(crate) async fn lock_pallet(
    transaction: &mut Transaction,
    pallet_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LockPalletRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            LOCK_PALLET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(pallet_id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(LOCK_PALLET_DIGEST, rows, |row| {
        Ok(LockPalletRow {
            location_id: row.decode("location_id")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn set_quantity(
    transaction: &mut Transaction,
    pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<Option<SetQuantityRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            SET_QUANTITY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(pallet_id),
                wamn_postgres_statements::into_sql_value(product_id),
                wamn_postgres_statements::into_sql_value(status),
                wamn_postgres_statements::into_sql_value(quantity),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_optional(SET_QUANTITY_DIGEST, rows, |row| {
        Ok(SetQuantityRow {
            id: row.decode("id")?,
            quantity: row.decode("quantity")?,
        })
    })
}

pub(crate) async fn touch_pallet(
    transaction: &mut Transaction,
    pallet_id: wamn_postgres_statements::Uuid,
) -> Result<TouchPalletRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            TOUCH_PALLET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(pallet_id)],
        )
        .await?;
    wamn_postgres_statements::decode_one(TOUCH_PALLET_DIGEST, rows, |row| {
        Ok(TouchPalletRow {
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}
