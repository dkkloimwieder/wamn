// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct AddToTargetRow {
    pub id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub(crate) struct ClaimCommandRow {
    pub movement_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct ConsumeSourceRow {
    pub row_version: i64,
}

#[derive(Debug)]
pub(crate) struct FinalizeCommandRow {
    pub row_version: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub movement_id: wamn_postgres_statements::Uuid,
    pub source_pallet_id: wamn_postgres_statements::Uuid,
    pub target_pallet_id: wamn_postgres_statements::Uuid,
    pub row_version: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct InsertMovementRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct LockBothPalletsRow {
    pub id: wamn_postgres_statements::Uuid,
    pub location_id: wamn_postgres_statements::Uuid,
    pub row_version: i64,
    pub status: String,
}

#[derive(Debug)]
pub(crate) struct PlaceOnTargetRow {
    pub id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub(crate) struct SelectSourceQuantityRow {
    pub product_id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
    pub status: String,
}

#[derive(Debug)]
pub(crate) struct TouchTargetRow {
    pub row_version: i64,
    pub status: String,
}

pub(crate) const ADD_TO_TARGET_DIGEST: &str = "sha256:0e3ab67e560416a19b68cc0003cdc1f52e97c5a9267ffa5224c64de8c6dd84b1";
pub(crate) const CLAIM_COMMAND_DIGEST: &str = "sha256:28b172e9d7ca08c84145653187ca6b234e21228f966a0b31209c9f00c4f4d9b2";
pub(crate) const CONSUME_SOURCE_DIGEST: &str = "sha256:605d3de9fd482e0e290cfc9c198f717ecd89f8a171e9e2b7b0be02b4c9dc562e";
pub(crate) const FINALIZE_COMMAND_DIGEST: &str = "sha256:c44c7e84564e5796433d1f1cece5709c6c1695d12e138cf00e297b2df0309a07";
pub(crate) const FIND_REPLAY_DIGEST: &str = "sha256:2db8575b15514d01b3a1dbdbd6e5663ace03c60bbe97b4c05becffc2d2e86266";
pub(crate) const INSERT_MOVEMENT_DIGEST: &str = "sha256:bb2256f51ee5bf3496731b4a77bf372e60d1cdad76aae282bab3ae17b7950229";
pub(crate) const LOCK_BOTH_PALLETS_DIGEST: &str = "sha256:1169fa9ccfdf21804049cb6c9698544af13f9d2e2460f8a871bfb5011e84480f";
pub(crate) const PLACE_ON_TARGET_DIGEST: &str = "sha256:7441c97e8175dadd886f000b59b6f1706a55e89f183b2e5bd4a154a7ba5b74f8";
pub(crate) const SELECT_SOURCE_QUANTITY_DIGEST: &str = "sha256:7788b618608496d40d21c0bbfec54e4508661fbea826075abb61e5cceeec6288";
pub(crate) const TOUCH_TARGET_DIGEST: &str = "sha256:520d48df78ae4ba6b655106f7785d0ca1e2887fcd3db489836f783e80ac88ea8";

pub(crate) async fn add_to_target(
    transaction: &mut Transaction,
    target_pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<Option<AddToTargetRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(ADD_TO_TARGET_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(target_pallet_id),
        wamn_postgres_statements::into_sql_value(product_id),
        wamn_postgres_statements::into_sql_value(status),
        wamn_postgres_statements::into_sql_value(quantity),
    ]).await?;
    wamn_postgres_statements::decode_optional(ADD_TO_TARGET_DIGEST, rows, |row| {
        Ok(AddToTargetRow {
            id: row.decode("id")?,
            quantity: row.decode("quantity")?,
        })
    })
}

pub(crate) async fn claim_command(
    transaction: &mut Transaction,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    source_pallet_id: wamn_postgres_statements::Uuid,
    target_pallet_id: wamn_postgres_statements::Uuid,
) -> Result<Option<ClaimCommandRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(CLAIM_COMMAND_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(canonical_command),
        wamn_postgres_statements::into_sql_value(source_pallet_id),
        wamn_postgres_statements::into_sql_value(target_pallet_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(CLAIM_COMMAND_DIGEST, rows, |row| {
        Ok(ClaimCommandRow {
            movement_id: row.decode("movement_id")?,
        })
    })
}

pub(crate) async fn consume_source(
    transaction: &mut Transaction,
    source_pallet_id: wamn_postgres_statements::Uuid,
) -> Result<ConsumeSourceRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(CONSUME_SOURCE_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(source_pallet_id),
    ]).await?;
    wamn_postgres_statements::decode_one(CONSUME_SOURCE_DIGEST, rows, |row| {
        Ok(ConsumeSourceRow {
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn finalize_command(
    transaction: &mut Transaction,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    movement_id: wamn_postgres_statements::Uuid,
    row_version: i64,
) -> Result<FinalizeCommandRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(FINALIZE_COMMAND_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(canonical_command),
        wamn_postgres_statements::into_sql_value(movement_id),
        wamn_postgres_statements::into_sql_value(row_version),
    ]).await?;
    wamn_postgres_statements::decode_one(FINALIZE_COMMAND_DIGEST, rows, |row| {
        Ok(FinalizeCommandRow {
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn find_replay(
    transaction: &mut Transaction,
    idempotency_key: String,
) -> Result<Option<FindReplayRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(FIND_REPLAY_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
    ]).await?;
    wamn_postgres_statements::decode_optional(FIND_REPLAY_DIGEST, rows, |row| {
        Ok(FindReplayRow {
            canonical_command: row.decode("canonical_command")?,
            movement_id: row.decode("movement_id")?,
            source_pallet_id: row.decode("source_pallet_id")?,
            target_pallet_id: row.decode("target_pallet_id")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn insert_movement(
    transaction: &mut Transaction,
    idempotency_key: String,
    pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    quantity: wamn_postgres_statements::Numeric,
    occurred_at: wamn_postgres_statements::TimestampTz,
) -> Result<InsertMovementRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(INSERT_MOVEMENT_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(pallet_id),
        wamn_postgres_statements::into_sql_value(product_id),
        wamn_postgres_statements::into_sql_value(quantity),
        wamn_postgres_statements::into_sql_value(occurred_at),
    ]).await?;
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
    let rows = transaction.run(LOCK_BOTH_PALLETS_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(source_pallet_id),
        wamn_postgres_statements::into_sql_value(target_pallet_id),
    ]).await?;
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
    let rows = transaction.run(PLACE_ON_TARGET_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(target_pallet_id),
        wamn_postgres_statements::into_sql_value(product_id),
        wamn_postgres_statements::into_sql_value(status),
        wamn_postgres_statements::into_sql_value(quantity),
    ]).await?;
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
    let rows = transaction.run(SELECT_SOURCE_QUANTITY_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(source_pallet_id),
    ]).await?;
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
    let rows = transaction.run(TOUCH_TARGET_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(target_pallet_id),
    ]).await?;
    wamn_postgres_statements::decode_one(TOUCH_TARGET_DIGEST, rows, |row| {
        Ok(TouchTargetRow {
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}
