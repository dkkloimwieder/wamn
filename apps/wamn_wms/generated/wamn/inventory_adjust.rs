// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct DeleteQuantityRow {
    pub id: wamn_postgres_statements::Uuid,
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
pub struct SelectQuantityRow {
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub struct SetQuantityRow {
    pub id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
}

#[derive(Debug)]
pub struct TouchPackagingRow {
    pub row_version: i32,
    pub status: String,
}

pub(crate) const DELETE_QUANTITY_DIGEST: &str =
    "sha256:86570346e5ce14bbb123d479da46a2fac3c4035d5cd6f2a799a9e1f300163420";
pub(crate) const INSERT_TRANSACTION_DIGEST: &str =
    "sha256:1b4db06a4d995a11f5835a69a27c7d4cf1238d4abcd72510a9d53a976257edd4";
pub(crate) const LOCK_PACKAGING_DIGEST: &str =
    "sha256:404783208c619d13cc5921442a94d7698cf002213e042b8db230086980a7c84a";
pub(crate) const SELECT_QUANTITY_DIGEST: &str =
    "sha256:7ad85cf1d8dded32d9e488711696067f61ae75356caf16464b0251defdb95b17";
pub(crate) const SET_QUANTITY_DIGEST: &str =
    "sha256:2fa2f6f71856ee72181ab45a8887015e3e8a135973d54034abc9abc684049b32";
pub(crate) const TOUCH_PACKAGING_DIGEST: &str =
    "sha256:6e09d7c04c57b23d8eaf4e2123ad967e090ec190ba8d1344c0d9c6a281ad2fc6";

pub(crate) async fn delete_quantity(
    transaction: &mut Transaction,
    packaging_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
) -> Result<DeleteQuantityRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            DELETE_QUANTITY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(packaging_id),
                wamn_postgres_statements::into_sql_value(product_id),
                wamn_postgres_statements::into_sql_value(status),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(DELETE_QUANTITY_DIGEST, rows, |row| {
        Ok(DeleteQuantityRow {
            id: row.decode("id")?,
        })
    })
}

pub(crate) async fn insert_transaction(
    transaction: &mut Transaction,
    packaging_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
    reason_code: String,
    occurred_at: wamn_postgres_statements::TimestampTz,
) -> Result<Option<InsertTransactionRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            INSERT_TRANSACTION_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(packaging_id),
                wamn_postgres_statements::into_sql_value(product_id),
                wamn_postgres_statements::into_sql_value(status),
                wamn_postgres_statements::into_sql_value(quantity),
                wamn_postgres_statements::into_sql_value(reason_code),
                wamn_postgres_statements::into_sql_value(occurred_at),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_optional(INSERT_TRANSACTION_DIGEST, rows, |row| {
        Ok(InsertTransactionRow {
            id: row.decode("id")?,
        })
    })
}

pub(crate) async fn lock_packaging(
    transaction: &mut Transaction,
    packaging_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LockPackagingRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            LOCK_PACKAGING_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(packaging_id)],
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

pub(crate) async fn select_quantity(
    transaction: &mut Transaction,
    packaging_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
) -> Result<Option<SelectQuantityRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            SELECT_QUANTITY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(packaging_id),
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

pub(crate) async fn set_quantity(
    transaction: &mut Transaction,
    packaging_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<Option<SetQuantityRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            SET_QUANTITY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(packaging_id),
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

pub(crate) async fn touch_packaging(
    transaction: &mut Transaction,
    packaging_id: wamn_postgres_statements::Uuid,
) -> Result<TouchPackagingRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            TOUCH_PACKAGING_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(packaging_id)],
        )
        .await?;
    wamn_postgres_statements::decode_one(TOUCH_PACKAGING_DIGEST, rows, |row| {
        Ok(TouchPackagingRow {
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}
