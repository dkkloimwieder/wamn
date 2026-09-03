// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct ClaimCommandRow {
    pub movement_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct FinalizeCommandRow {
    pub adjusted_quantity: Option<wamn_postgres_statements::Numeric>,
    pub row_version: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub movement_id: wamn_postgres_statements::Uuid,
    pub pallet_id: wamn_postgres_statements::Uuid,
    pub adjusted_quantity: Option<wamn_postgres_statements::Numeric>,
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
pub(crate) struct SetQuantityRow {
    pub id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub(crate) struct TouchPalletRow {
    pub row_version: i64,
    pub status: String,
}

pub(crate) const CLAIM_COMMAND_DIGEST: &str = "sha256:bfc2b9ff0c01e1d082ba1858c040fd8f5705ce80cbc642edde939502c6da2e7c";
pub(crate) const FINALIZE_COMMAND_DIGEST: &str = "sha256:95fceeb3dd95c108d959511e94edbb52da891da2111910aa1b25b70181f319cf";
pub(crate) const FIND_REPLAY_DIGEST: &str = "sha256:982241a393ffb1d4289136cbee42de40f91dda86a47188c34665eed34ecd69e6";
pub(crate) const INSERT_MOVEMENT_DIGEST: &str = "sha256:cd00a091fed66f2f78e28f58d1264017a2b105a2666cfddd5b288f5a8a99c940";
pub(crate) const LOCK_PALLET_DIGEST: &str = "sha256:a55bfebbebf5bba9540074165b1e5750116fda67b8ef07439c89469ed1ffece3";
pub(crate) const SET_QUANTITY_DIGEST: &str = "sha256:013414bd90ba990f46f429326fbb956c10e709e3a355ee5f91fcea7d82c3ef86";
pub(crate) const TOUCH_PALLET_DIGEST: &str = "sha256:520d48df78ae4ba6b655106f7785d0ca1e2887fcd3db489836f783e80ac88ea8";

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
    adjusted_quantity: wamn_postgres_statements::Numeric,
    row_version: i64,
) -> Result<FinalizeCommandRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(FINALIZE_COMMAND_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(canonical_command),
        wamn_postgres_statements::into_sql_value(movement_id),
        wamn_postgres_statements::into_sql_value(adjusted_quantity),
        wamn_postgres_statements::into_sql_value(row_version),
    ]).await?;
    wamn_postgres_statements::decode_one(FINALIZE_COMMAND_DIGEST, rows, |row| {
        Ok(FinalizeCommandRow {
            adjusted_quantity: row.decode("adjusted_quantity")?,
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
            adjusted_quantity: row.decode("adjusted_quantity")?,
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
    reason_code: String,
    occurred_at: wamn_postgres_statements::TimestampTz,
) -> Result<InsertMovementRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(INSERT_MOVEMENT_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(pallet_id),
        wamn_postgres_statements::into_sql_value(product_id),
        wamn_postgres_statements::into_sql_value(quantity),
        wamn_postgres_statements::into_sql_value(reason_code),
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

pub(crate) async fn set_quantity(
    transaction: &mut Transaction,
    pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<Option<SetQuantityRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(SET_QUANTITY_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(pallet_id),
        wamn_postgres_statements::into_sql_value(product_id),
        wamn_postgres_statements::into_sql_value(status),
        wamn_postgres_statements::into_sql_value(quantity),
    ]).await?;
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
    let rows = transaction.run(TOUCH_PALLET_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(pallet_id),
    ]).await?;
    wamn_postgres_statements::decode_one(TOUCH_PALLET_DIGEST, rows, |row| {
        Ok(TouchPalletRow {
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}
