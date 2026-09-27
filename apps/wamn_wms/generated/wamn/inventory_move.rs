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
pub struct MovePalletRow {
    pub location_id: wamn_postgres_statements::Uuid,
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug)]
pub struct SelectPalletQuantityRow {
    pub product_id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
    pub status: String,
}

#[derive(Debug)]
pub struct ValidateLocationRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const INSERT_MOVEMENT_DIGEST: &str =
    "sha256:6fe8b4f1193308a70594184884ceea1640a5bbbee1a3eb1ebc313d276ea200af";
pub(crate) const LOCK_PALLET_DIGEST: &str =
    "sha256:a55bfebbebf5bba9540074165b1e5750116fda67b8ef07439c89469ed1ffece3";
pub(crate) const MOVE_PALLET_DIGEST: &str =
    "sha256:feffc4e473ca661e1d446a2a928eb3bed2d5f45ebeadf07ddf7808874e6b3af9";
pub(crate) const SELECT_PALLET_QUANTITY_DIGEST: &str =
    "sha256:7788b618608496d40d21c0bbfec54e4508661fbea826075abb61e5cceeec6288";
pub(crate) const VALIDATE_LOCATION_DIGEST: &str =
    "sha256:043f1cb7e8359f79c83b7944e308c1d4238a2bc7b0eac50a0093e53e7563d516";

pub(crate) async fn insert_movement(
    transaction: &mut Transaction,
    pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    from_location_id: wamn_postgres_statements::Uuid,
    to_location_id: wamn_postgres_statements::Uuid,
    quantity: wamn_postgres_statements::Numeric,
    occurred_at: wamn_postgres_statements::TimestampTz,
) -> Result<InsertMovementRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            INSERT_MOVEMENT_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(pallet_id),
                wamn_postgres_statements::into_sql_value(product_id),
                wamn_postgres_statements::into_sql_value(from_location_id),
                wamn_postgres_statements::into_sql_value(to_location_id),
                wamn_postgres_statements::into_sql_value(quantity),
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

pub(crate) async fn move_pallet(
    transaction: &mut Transaction,
    pallet_id: wamn_postgres_statements::Uuid,
    to_location_id: wamn_postgres_statements::Uuid,
) -> Result<MovePalletRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            MOVE_PALLET_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(pallet_id),
                wamn_postgres_statements::into_sql_value(to_location_id),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(MOVE_PALLET_DIGEST, rows, |row| {
        Ok(MovePalletRow {
            location_id: row.decode("location_id")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn select_pallet_quantity(
    transaction: &mut Transaction,
    pallet_id: wamn_postgres_statements::Uuid,
) -> Result<Vec<SelectPalletQuantityRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            SELECT_PALLET_QUANTITY_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(pallet_id)],
        )
        .await?;
    wamn_postgres_statements::decode_all(SELECT_PALLET_QUANTITY_DIGEST, rows, |row| {
        Ok(SelectPalletQuantityRow {
            product_id: row.decode("product_id")?,
            quantity: row.decode("quantity")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn validate_location(
    transaction: &mut Transaction,
    location_id: wamn_postgres_statements::Uuid,
) -> Result<Option<ValidateLocationRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            VALIDATE_LOCATION_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(location_id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(VALIDATE_LOCATION_DIGEST, rows, |row| {
        Ok(ValidateLocationRow {
            id: row.decode("id")?,
        })
    })
}
