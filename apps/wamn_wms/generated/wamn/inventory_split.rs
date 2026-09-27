// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct CreatePalletRow {
    pub id: wamn_postgres_statements::Uuid,
    pub row_version: i32,
    pub status: String,
}

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
pub struct PlaceQuantityRow {
    pub id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub struct SelectQuantityRow {
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub struct TakeFromSourceRow {
    pub id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub struct TouchSourceRow {
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug)]
pub struct ValidateLocationRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const CREATE_PALLET_DIGEST: &str =
    "sha256:7ee2a79c3a4206b875f451003812d8a4c4d51620002d6499439556d3b03aa008";
pub(crate) const INSERT_MOVEMENT_DIGEST: &str =
    "sha256:e973cf72f331518b67d4d589791f2c78de9ad97ffb153722f6dd90ded682e016";
pub(crate) const LOCK_PALLET_DIGEST: &str =
    "sha256:a55bfebbebf5bba9540074165b1e5750116fda67b8ef07439c89469ed1ffece3";
pub(crate) const PLACE_QUANTITY_DIGEST: &str =
    "sha256:7441c97e8175dadd886f000b59b6f1706a55e89f183b2e5bd4a154a7ba5b74f8";
pub(crate) const SELECT_QUANTITY_DIGEST: &str =
    "sha256:9e8cae601ec91090165e5d3e72999e7c7aaf5937c7e0ec1b8c17f52b7d54d4f3";
pub(crate) const TAKE_FROM_SOURCE_DIGEST: &str =
    "sha256:d2d1f7e49de0b5cb74c0d1d672bb9cf8a50e930b529463f93d054f5f5145467c";
pub(crate) const TOUCH_SOURCE_DIGEST: &str =
    "sha256:1015523c5b8bfcec2e8b84ed44c5b1a20fb39209656c0f78f3f68408850070d9";
pub(crate) const VALIDATE_LOCATION_DIGEST: &str =
    "sha256:043f1cb7e8359f79c83b7944e308c1d4238a2bc7b0eac50a0093e53e7563d516";

pub(crate) async fn create_pallet(
    transaction: &mut Transaction,
    new_pallet_code: String,
    to_location_id: wamn_postgres_statements::Uuid,
    status: String,
) -> Result<CreatePalletRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            CREATE_PALLET_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(new_pallet_code),
                wamn_postgres_statements::into_sql_value(to_location_id),
                wamn_postgres_statements::into_sql_value(status),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(CREATE_PALLET_DIGEST, rows, |row| {
        Ok(CreatePalletRow {
            id: row.decode("id")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
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

pub(crate) async fn lock_pallet(
    transaction: &mut Transaction,
    source_pallet_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LockPalletRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            LOCK_PALLET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(source_pallet_id)],
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

pub(crate) async fn place_quantity(
    transaction: &mut Transaction,
    new_pallet_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<PlaceQuantityRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            PLACE_QUANTITY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(new_pallet_id),
                wamn_postgres_statements::into_sql_value(product_id),
                wamn_postgres_statements::into_sql_value(status),
                wamn_postgres_statements::into_sql_value(quantity),
            ],
        )
        .await?;
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
    let rows = transaction
        .run(
            SELECT_QUANTITY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(source_pallet_id),
                wamn_postgres_statements::into_sql_value(product_id),
                wamn_postgres_statements::into_sql_value(status),
            ],
        )
        .await?;
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
    let rows = transaction
        .run(
            TAKE_FROM_SOURCE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(source_pallet_id),
                wamn_postgres_statements::into_sql_value(product_id),
                wamn_postgres_statements::into_sql_value(status),
                wamn_postgres_statements::into_sql_value(quantity),
            ],
        )
        .await?;
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
    let rows = transaction
        .run(
            TOUCH_SOURCE_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(source_pallet_id)],
        )
        .await?;
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
    let rows = transaction
        .run(
            VALIDATE_LOCATION_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(to_location_id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(VALIDATE_LOCATION_DIGEST, rows, |row| {
        Ok(ValidateLocationRow {
            id: row.decode("id")?,
        })
    })
}
