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
pub struct DeleteSourceQuantityRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct InsertTransactionRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct LockBothPackagingsRow {
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
    "sha256:8dab969420bcf1959995bf2864bd40782a357aa6315f562de255baaf3514b35c";
pub(crate) const CONSUME_SOURCE_DIGEST: &str =
    "sha256:03c5376d3a35d4521796d0d23ba50a2e92b72291a727d88d696b95ed20a971fc";
pub(crate) const DELETE_SOURCE_QUANTITY_DIGEST: &str =
    "sha256:b82f10c54606d22a923f1a11d93785a67e4ebccd7ff157013f4506970cb3fc21";
pub(crate) const INSERT_TRANSACTION_DIGEST: &str =
    "sha256:210fc0bc3e8fded3c634d68e608a4999f01bd521c5b98c88b7c453d815cfcb43";
pub(crate) const LOCK_BOTH_PACKAGINGS_DIGEST: &str =
    "sha256:a03a0f69fa75d8d327dd2c3879ee0dd96907b0ee5cd13d8dda772d40a45dcd9e";
pub(crate) const PLACE_ON_TARGET_DIGEST: &str =
    "sha256:9403eefd95ab8c0a81cfa2672e49af12b1d4509e3d4f931ef53c6e6163b39488";
pub(crate) const SELECT_SOURCE_QUANTITY_DIGEST: &str =
    "sha256:7ecfac2dde679fc54e3133190a945c2418de98401fb4c0e2274ddea42b708d18";
pub(crate) const TOUCH_TARGET_DIGEST: &str =
    "sha256:6e09d7c04c57b23d8eaf4e2123ad967e090ec190ba8d1344c0d9c6a281ad2fc6";

pub(crate) async fn add_to_target(
    transaction: &mut Transaction,
    target_packaging_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<Option<AddToTargetRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            ADD_TO_TARGET_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(target_packaging_id),
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
    source_packaging_id: wamn_postgres_statements::Uuid,
) -> Result<ConsumeSourceRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            CONSUME_SOURCE_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(
                source_packaging_id,
            )],
        )
        .await?;
    wamn_postgres_statements::decode_one(CONSUME_SOURCE_DIGEST, rows, |row| {
        Ok(ConsumeSourceRow {
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn delete_source_quantity(
    transaction: &mut Transaction,
    source_packaging_id: wamn_postgres_statements::Uuid,
) -> Result<Vec<DeleteSourceQuantityRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            DELETE_SOURCE_QUANTITY_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(
                source_packaging_id,
            )],
        )
        .await?;
    wamn_postgres_statements::decode_all(DELETE_SOURCE_QUANTITY_DIGEST, rows, |row| {
        Ok(DeleteSourceQuantityRow {
            id: row.decode("id")?,
        })
    })
}

pub(crate) async fn insert_transaction(
    transaction: &mut Transaction,
    product_id: wamn_postgres_statements::Uuid,
    quantity: wamn_postgres_statements::Numeric,
    source_packaging_id: wamn_postgres_statements::Uuid,
    status: String,
    target_packaging_id: wamn_postgres_statements::Uuid,
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
                wamn_postgres_statements::into_sql_value(target_packaging_id),
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

pub(crate) async fn lock_both_packagings(
    transaction: &mut Transaction,
    source_packaging_id: wamn_postgres_statements::Uuid,
    target_packaging_id: wamn_postgres_statements::Uuid,
) -> Result<Vec<LockBothPackagingsRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            LOCK_BOTH_PACKAGINGS_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(source_packaging_id),
                wamn_postgres_statements::into_sql_value(target_packaging_id),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(LOCK_BOTH_PACKAGINGS_DIGEST, rows, |row| {
        Ok(LockBothPackagingsRow {
            id: row.decode("id")?,
            location_id: row.decode("location_id")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn place_on_target(
    transaction: &mut Transaction,
    target_packaging_id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    status: String,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<PlaceOnTargetRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            PLACE_ON_TARGET_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(target_packaging_id),
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
    source_packaging_id: wamn_postgres_statements::Uuid,
) -> Result<Vec<SelectSourceQuantityRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            SELECT_SOURCE_QUANTITY_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(
                source_packaging_id,
            )],
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
    target_packaging_id: wamn_postgres_statements::Uuid,
) -> Result<TouchTargetRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            TOUCH_TARGET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(
                target_packaging_id,
            )],
        )
        .await?;
    wamn_postgres_statements::decode_one(TOUCH_TARGET_DIGEST, rows, |row| {
        Ok(TouchTargetRow {
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}
