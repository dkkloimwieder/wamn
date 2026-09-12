// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct ClaimCommandRow {
    pub movement_id: wamn_postgres_statements::Uuid,
    pub new_pallet_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct CreatePalletRow {
    pub id: wamn_postgres_statements::Uuid,
    pub row_version: i64,
    pub status: String,
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
    pub new_pallet_id: wamn_postgres_statements::Uuid,
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
pub(crate) struct PlaceQuantityRow {
    pub id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub(crate) struct SelectQuantityRow {
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub(crate) struct TakeFromSourceRow {
    pub id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub(crate) struct TouchSourceRow {
    pub row_version: i64,
    pub status: String,
}

#[derive(Debug)]
pub(crate) struct ValidateLocationRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const CLAIM_COMMAND_DIGEST: &str = "sha256:2256a274724c0d79beee91fb5f94f0963c428848099dfbd226833dcb9d943a67";
pub(crate) const CREATE_PALLET_DIGEST: &str = "sha256:1e17d1f7ab74aecb89b4da78a6e946859719a78eb07af7d52efb2f6c0a8a52c6";
pub(crate) const FINALIZE_COMMAND_DIGEST: &str = "sha256:1193cc2835abd0b43372c07d60852e801bb579b85a34f4480d1199bd485f9ed8";
pub(crate) const FIND_REPLAY_DIGEST: &str = "sha256:38c3a3247c43b4fd938d7a66737f80bd31e45ab2ce676e169f2d4143dfebb810";
pub(crate) const INSERT_MOVEMENT_DIGEST: &str = "sha256:e4ccda97b9ef5cb01093515c83086ba546dec9f46691c2f3ca477764991ad3ef";
pub(crate) const LOCK_PALLET_DIGEST: &str = "sha256:a55bfebbebf5bba9540074165b1e5750116fda67b8ef07439c89469ed1ffece3";
pub(crate) const PLACE_QUANTITY_DIGEST: &str = "sha256:7441c97e8175dadd886f000b59b6f1706a55e89f183b2e5bd4a154a7ba5b74f8";
pub(crate) const SELECT_QUANTITY_DIGEST: &str = "sha256:9e8cae601ec91090165e5d3e72999e7c7aaf5937c7e0ec1b8c17f52b7d54d4f3";
pub(crate) const TAKE_FROM_SOURCE_DIGEST: &str = "sha256:d2d1f7e49de0b5cb74c0d1d672bb9cf8a50e930b529463f93d054f5f5145467c";
pub(crate) const TOUCH_SOURCE_DIGEST: &str = "sha256:520d48df78ae4ba6b655106f7785d0ca1e2887fcd3db489836f783e80ac88ea8";
pub(crate) const VALIDATE_LOCATION_DIGEST: &str = "sha256:043f1cb7e8359f79c83b7944e308c1d4238a2bc7b0eac50a0093e53e7563d516";

pub(crate) async fn claim_command(
    transaction: &mut Transaction,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    source_pallet_id: wamn_postgres_statements::Uuid,
) -> Result<Option<ClaimCommandRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(CLAIM_COMMAND_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(canonical_command),
        wamn_postgres_statements::into_sql_value(source_pallet_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(CLAIM_COMMAND_DIGEST, rows, |row| {
        Ok(ClaimCommandRow {
            movement_id: row.decode("movement_id")?,
            new_pallet_id: row.decode("new_pallet_id")?,
        })
    })
}

pub(crate) async fn create_pallet(
    transaction: &mut Transaction,
    new_pallet_id: wamn_postgres_statements::Uuid,
    new_pallet_code: String,
    to_location_id: wamn_postgres_statements::Uuid,
    status: String,
) -> Result<CreatePalletRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(CREATE_PALLET_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(new_pallet_id),
        wamn_postgres_statements::into_sql_value(new_pallet_code),
        wamn_postgres_statements::into_sql_value(to_location_id),
        wamn_postgres_statements::into_sql_value(status),
    ]).await?;
    wamn_postgres_statements::decode_one(CREATE_PALLET_DIGEST, rows, |row| {
        Ok(CreatePalletRow {
            id: row.decode("id")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
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
            new_pallet_id: row.decode("new_pallet_id")?,
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

pub(crate) async fn lock_pallet(
    transaction: &mut Transaction,
    source_pallet_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LockPalletRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(LOCK_PALLET_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(source_pallet_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(LOCK_PALLET_DIGEST, rows, |row| {
        Ok(LockPalletRow {
            location_id: row.decode("location_id")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn place_quantity(
    transaction: &mut Transaction,
    new_pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<PlaceQuantityRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(PLACE_QUANTITY_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(new_pallet_id),
        wamn_postgres_statements::into_sql_value(product_id),
        wamn_postgres_statements::into_sql_value(status),
        wamn_postgres_statements::into_sql_value(quantity),
    ]).await?;
    wamn_postgres_statements::decode_one(PLACE_QUANTITY_DIGEST, rows, |row| {
        Ok(PlaceQuantityRow {
            id: row.decode("id")?,
            quantity: row.decode("quantity")?,
        })
    })
}

pub(crate) async fn select_quantity(
    transaction: &mut Transaction,
    source_pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
) -> Result<Option<SelectQuantityRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(SELECT_QUANTITY_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(source_pallet_id),
        wamn_postgres_statements::into_sql_value(product_id),
        wamn_postgres_statements::into_sql_value(status),
    ]).await?;
    wamn_postgres_statements::decode_optional(SELECT_QUANTITY_DIGEST, rows, |row| {
        Ok(SelectQuantityRow {
            quantity: row.decode("quantity")?,
        })
    })
}

pub(crate) async fn take_from_source(
    transaction: &mut Transaction,
    source_pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<Option<TakeFromSourceRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(TAKE_FROM_SOURCE_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(source_pallet_id),
        wamn_postgres_statements::into_sql_value(product_id),
        wamn_postgres_statements::into_sql_value(status),
        wamn_postgres_statements::into_sql_value(quantity),
    ]).await?;
    wamn_postgres_statements::decode_optional(TAKE_FROM_SOURCE_DIGEST, rows, |row| {
        Ok(TakeFromSourceRow {
            id: row.decode("id")?,
            quantity: row.decode("quantity")?,
        })
    })
}

pub(crate) async fn touch_source(
    transaction: &mut Transaction,
    source_pallet_id: wamn_postgres_statements::Uuid,
) -> Result<TouchSourceRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(TOUCH_SOURCE_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(source_pallet_id),
    ]).await?;
    wamn_postgres_statements::decode_one(TOUCH_SOURCE_DIGEST, rows, |row| {
        Ok(TouchSourceRow {
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn validate_location(
    transaction: &mut Transaction,
    to_location_id: wamn_postgres_statements::Uuid,
) -> Result<Option<ValidateLocationRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(VALIDATE_LOCATION_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(to_location_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(VALIDATE_LOCATION_DIGEST, rows, |row| {
        Ok(ValidateLocationRow {
            id: row.decode("id")?,
        })
    })
}
