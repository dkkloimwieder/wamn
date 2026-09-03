// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct ClaimCommandRow {
    pub movement_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct FinalizeCommandRow {
    pub pallet_status: Option<String>,
    pub row_version: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub movement_id: wamn_postgres_statements::Uuid,
    pub pallet_id: wamn_postgres_statements::Uuid,
    pub pallet_status: Option<String>,
    pub row_version: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct InsertMovementRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct LockPalletRow {
    pub location_id: wamn_postgres_statements::Uuid,
    pub row_version: i64,
    pub status: String,
}

#[derive(Debug)]
pub(crate) struct MovePalletRow {
    pub row_version: i64,
    pub status: String,
}

#[derive(Debug)]
pub(crate) struct SelectPalletQuantityRow {
    pub product_id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
    pub status: String,
}

#[derive(Debug)]
pub(crate) struct ValidateLocationRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const CLAIM_COMMAND_DIGEST: &str = "sha256:b919dc1461addbaf6b2ec0e3f5f383db196ef48ab2e6f31328ceddc0eb027010";
pub(crate) const FINALIZE_COMMAND_DIGEST: &str = "sha256:25ad208233b35158b2a8b6e86a2168259833ddefe767314378bef992efa4113e";
pub(crate) const FIND_REPLAY_DIGEST: &str = "sha256:1de97cab3802dcc898f61fc86c29fb776b9b33cd81b080b6d1567fa48e449b54";
pub(crate) const INSERT_MOVEMENT_DIGEST: &str = "sha256:4ecddcc7be1836213dd4b10c5e039ae2f6a10e590e4228131a1ffffb41b978a7";
pub(crate) const LOCK_PALLET_DIGEST: &str = "sha256:a55bfebbebf5bba9540074165b1e5750116fda67b8ef07439c89469ed1ffece3";
pub(crate) const MOVE_PALLET_DIGEST: &str = "sha256:64beda453260a3bde8bb9c40a47aedafe3fef62beb5f73bda38baa743402ef36";
pub(crate) const SELECT_PALLET_QUANTITY_DIGEST: &str = "sha256:7788b618608496d40d21c0bbfec54e4508661fbea826075abb61e5cceeec6288";
pub(crate) const VALIDATE_LOCATION_DIGEST: &str = "sha256:043f1cb7e8359f79c83b7944e308c1d4238a2bc7b0eac50a0093e53e7563d516";

pub(crate) async fn claim_command(
    transaction: &mut Transaction,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    pallet_id: wamn_postgres_statements::Uuid,
) -> Result<Option<ClaimCommandRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(CLAIM_COMMAND_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(canonical_command),
        wamn_postgres_statements::into_sql_value(pallet_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(CLAIM_COMMAND_DIGEST, rows, |row| {
        Ok(ClaimCommandRow {
            movement_id: row.decode("movement_id")?,
        })
    })
}

pub(crate) async fn finalize_command(
    transaction: &mut Transaction,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    movement_id: wamn_postgres_statements::Uuid,
    pallet_status: String,
    row_version: i64,
) -> Result<FinalizeCommandRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(FINALIZE_COMMAND_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(canonical_command),
        wamn_postgres_statements::into_sql_value(movement_id),
        wamn_postgres_statements::into_sql_value(pallet_status),
        wamn_postgres_statements::into_sql_value(row_version),
    ]).await?;
    wamn_postgres_statements::decode_one(FINALIZE_COMMAND_DIGEST, rows, |row| {
        Ok(FinalizeCommandRow {
            pallet_status: row.decode("pallet_status")?,
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
            pallet_id: row.decode("pallet_id")?,
            pallet_status: row.decode("pallet_status")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn insert_movement(
    transaction: &mut Transaction,
    idempotency_key: String,
    pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    from_location_id: wamn_postgres_statements::Uuid,
    to_location_id: wamn_postgres_statements::Uuid,
    quantity: wamn_postgres_statements::Numeric,
    occurred_at: wamn_postgres_statements::TimestampTz,
) -> Result<InsertMovementRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(INSERT_MOVEMENT_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(pallet_id),
        wamn_postgres_statements::into_sql_value(product_id),
        wamn_postgres_statements::into_sql_value(from_location_id),
        wamn_postgres_statements::into_sql_value(to_location_id),
        wamn_postgres_statements::into_sql_value(quantity),
        wamn_postgres_statements::into_sql_value(occurred_at),
    ]).await?;
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
    let rows = transaction.run(LOCK_PALLET_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(pallet_id),
    ]).await?;
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
    let rows = transaction.run(MOVE_PALLET_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(pallet_id),
        wamn_postgres_statements::into_sql_value(to_location_id),
    ]).await?;
    wamn_postgres_statements::decode_one(MOVE_PALLET_DIGEST, rows, |row| {
        Ok(MovePalletRow {
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn select_pallet_quantity(
    transaction: &mut Transaction,
    pallet_id: wamn_postgres_statements::Uuid,
) -> Result<Vec<SelectPalletQuantityRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(SELECT_PALLET_QUANTITY_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(pallet_id),
    ]).await?;
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
    let rows = transaction.run(VALIDATE_LOCATION_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(location_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(VALIDATE_LOCATION_DIGEST, rows, |row| {
        Ok(ValidateLocationRow {
            id: row.decode("id")?,
        })
    })
}
