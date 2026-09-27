// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct AddToTargetRow {
    pub id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub struct ConsumeSourceRow {
    pub row_version: i32,
}

#[derive(Debug)]
pub struct InsertMovementRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct LockBothPalletsRow {
    pub id: wamn_postgres_statements::Uuid,
    pub location_id: wamn_postgres_statements::Uuid,
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug)]
pub struct PlaceOnTargetRow {
    pub id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub struct SelectSourceQuantityRow {
    pub product_id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
    pub status: String,
}

#[derive(Debug)]
pub struct TouchTargetRow {
    pub row_version: i32,
    pub status: String,
}

pub(crate) const ADD_TO_TARGET_DIGEST: &str =
    "sha256:0e3ab67e560416a19b68cc0003cdc1f52e97c5a9267ffa5224c64de8c6dd84b1";
pub(crate) const CONSUME_SOURCE_DIGEST: &str =
    "sha256:92b0585255ca8e6949e1f8a4f1c32459a92c9f7a7145fc0e80d74c57d585d2d6";
pub(crate) const INSERT_MOVEMENT_DIGEST: &str =
    "sha256:3d79467cf8e530bc2f7293fe0f77ee44e44d416f8128260e840fc052ce32d633";
pub(crate) const LOCK_BOTH_PALLETS_DIGEST: &str =
    "sha256:1169fa9ccfdf21804049cb6c9698544af13f9d2e2460f8a871bfb5011e84480f";
pub(crate) const PLACE_ON_TARGET_DIGEST: &str =
    "sha256:7441c97e8175dadd886f000b59b6f1706a55e89f183b2e5bd4a154a7ba5b74f8";
pub(crate) const SELECT_SOURCE_QUANTITY_DIGEST: &str =
    "sha256:7788b618608496d40d21c0bbfec54e4508661fbea826075abb61e5cceeec6288";
pub(crate) const TOUCH_TARGET_DIGEST: &str =
    "sha256:1015523c5b8bfcec2e8b84ed44c5b1a20fb39209656c0f78f3f68408850070d9";

pub(crate) async fn add_to_target(
    transaction: &mut Transaction,
    target_pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<Option<AddToTargetRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            ADD_TO_TARGET_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(target_pallet_id),
                wamn_postgres_statements::into_sql_value(product_id),
                wamn_postgres_statements::into_sql_value(status),
                wamn_postgres_statements::into_sql_value(quantity),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_optional(ADD_TO_TARGET_DIGEST, rows, |row| {
        Ok(AddToTargetRow {
            id: row.decode("id")?,
            quantity: row.decode("quantity")?,
        })
    })
}

pub(crate) async fn consume_source(
    transaction: &mut Transaction,
    source_pallet_id: wamn_postgres_statements::Uuid,
) -> Result<ConsumeSourceRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            CONSUME_SOURCE_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(source_pallet_id)],
        )
        .await?;
    wamn_postgres_statements::decode_one(CONSUME_SOURCE_DIGEST, rows, |row| {
        Ok(ConsumeSourceRow {
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn insert_movement(
    transaction: &mut Transaction,
    pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    quantity: wamn_postgres_statements::Numeric,
    occurred_at: wamn_postgres_statements::TimestampTz,
) -> Result<InsertMovementRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            INSERT_MOVEMENT_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(pallet_id),
                wamn_postgres_statements::into_sql_value(product_id),
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

pub(crate) async fn lock_both_pallets(
    transaction: &mut Transaction,
    source_pallet_id: wamn_postgres_statements::Uuid,
    target_pallet_id: wamn_postgres_statements::Uuid,
) -> Result<Vec<LockBothPalletsRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            LOCK_BOTH_PALLETS_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(source_pallet_id),
                wamn_postgres_statements::into_sql_value(target_pallet_id),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(LOCK_BOTH_PALLETS_DIGEST, rows, |row| {
        Ok(LockBothPalletsRow {
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn place_on_target(
    transaction: &mut Transaction,
    target_pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<PlaceOnTargetRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            PLACE_ON_TARGET_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(target_pallet_id),
                wamn_postgres_statements::into_sql_value(product_id),
                wamn_postgres_statements::into_sql_value(status),
                wamn_postgres_statements::into_sql_value(quantity),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(PLACE_ON_TARGET_DIGEST, rows, |row| {
        Ok(PlaceOnTargetRow {
            id: row.decode("id")?,
            quantity: row.decode("quantity")?,
        })
    })
}

pub(crate) async fn select_source_quantity(
    transaction: &mut Transaction,
    source_pallet_id: wamn_postgres_statements::Uuid,
) -> Result<Vec<SelectSourceQuantityRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            SELECT_SOURCE_QUANTITY_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(source_pallet_id)],
        )
        .await?;
    wamn_postgres_statements::decode_all(SELECT_SOURCE_QUANTITY_DIGEST, rows, |row| {
        Ok(SelectSourceQuantityRow {
            product_id: row.decode("product_id")?,
            quantity: row.decode("quantity")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn touch_target(
    transaction: &mut Transaction,
    target_pallet_id: wamn_postgres_statements::Uuid,
) -> Result<TouchTargetRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            TOUCH_TARGET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(target_pallet_id)],
        )
        .await?;
    wamn_postgres_statements::decode_one(TOUCH_TARGET_DIGEST, rows, |row| {
        Ok(TouchTargetRow {
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}
