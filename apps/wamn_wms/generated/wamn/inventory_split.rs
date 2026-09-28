// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct CreatePackagingRow {
    pub id: wamn_postgres_statements::Uuid,
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug)]
pub struct InsertTransactionRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct LockPackagingRow {
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

pub(crate) const CREATE_PACKAGING_DIGEST: &str =
    "sha256:fc9146bbfc41e5a3351b3b6dab97ed8021e3837b04d6875dee8c417de68aafaa";
pub(crate) const INSERT_TRANSACTION_DIGEST: &str =
    "sha256:210fc0bc3e8fded3c634d68e608a4999f01bd521c5b98c88b7c453d815cfcb43";
pub(crate) const LOCK_PACKAGING_DIGEST: &str =
    "sha256:404783208c619d13cc5921442a94d7698cf002213e042b8db230086980a7c84a";
pub(crate) const PLACE_QUANTITY_DIGEST: &str =
    "sha256:9403eefd95ab8c0a81cfa2672e49af12b1d4509e3d4f931ef53c6e6163b39488";
pub(crate) const SELECT_QUANTITY_DIGEST: &str =
    "sha256:7d7cac58427384ec1b1a9e648bef5a501f0c3a9684de5d1b52067135613f5411";
pub(crate) const TAKE_FROM_SOURCE_DIGEST: &str =
    "sha256:b4740f3f3b70f15b6eb2e618813b8d153af8e2b43e4b7352c00e00194b129f71";
pub(crate) const TOUCH_SOURCE_DIGEST: &str =
    "sha256:6e09d7c04c57b23d8eaf4e2123ad967e090ec190ba8d1344c0d9c6a281ad2fc6";
pub(crate) const VALIDATE_LOCATION_DIGEST: &str =
    "sha256:043f1cb7e8359f79c83b7944e308c1d4238a2bc7b0eac50a0093e53e7563d516";

pub(crate) async fn create_packaging(
    transaction: &mut Transaction,
    new_packaging_code: String,
    new_packaging_type: String,
    to_location_id: wamn_postgres_statements::Uuid,
    occurred_at: wamn_postgres_statements::TimestampTz,
    status: String,
) -> Result<CreatePackagingRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            CREATE_PACKAGING_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(new_packaging_code),
                wamn_postgres_statements::into_sql_value(new_packaging_type),
                wamn_postgres_statements::into_sql_value(to_location_id),
                wamn_postgres_statements::into_sql_value(occurred_at),
                wamn_postgres_statements::into_sql_value(status),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(CREATE_PACKAGING_DIGEST, rows, |row| {
        Ok(CreatePackagingRow {
            id: row.decode("id")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn insert_transaction(
    transaction: &mut Transaction,
    product_id: wamn_postgres_statements::Uuid,
    quantity: wamn_postgres_statements::Numeric,
    source_packaging_id: wamn_postgres_statements::Uuid,
    status: String,
    new_packaging_id: wamn_postgres_statements::Uuid,
    occurred_at: wamn_postgres_statements::TimestampTz,
) -> Result<InsertTransactionRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            INSERT_TRANSACTION_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(product_id),
                wamn_postgres_statements::into_sql_value(quantity),
                wamn_postgres_statements::into_sql_value(source_packaging_id),
                wamn_postgres_statements::into_sql_value(status),
                wamn_postgres_statements::into_sql_value(new_packaging_id),
                wamn_postgres_statements::into_sql_value(occurred_at),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(INSERT_TRANSACTION_DIGEST, rows, |row| {
        Ok(InsertTransactionRow {
            id: row.decode("id")?,
        })
    })
}

pub(crate) async fn lock_packaging(
    transaction: &mut Transaction,
    source_packaging_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LockPackagingRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            LOCK_PACKAGING_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(
                source_packaging_id,
            )],
        )
        .await?;
    wamn_postgres_statements::decode_optional(LOCK_PACKAGING_DIGEST, rows, |row| {
        Ok(LockPackagingRow {
            location_id: row.decode("location_id")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn place_quantity(
    transaction: &mut Transaction,
    new_packaging_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<PlaceQuantityRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            PLACE_QUANTITY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(new_packaging_id),
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
    source_packaging_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
) -> Result<Option<SelectQuantityRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            SELECT_QUANTITY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(source_packaging_id),
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
    source_packaging_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<Option<TakeFromSourceRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            TAKE_FROM_SOURCE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(source_packaging_id),
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
    source_packaging_id: wamn_postgres_statements::Uuid,
) -> Result<TouchSourceRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            TOUCH_SOURCE_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(
                source_packaging_id,
            )],
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
