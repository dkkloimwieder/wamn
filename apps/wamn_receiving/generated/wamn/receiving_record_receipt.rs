// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct FinishPurchaseOrderRow {
    pub status: String,
    pub row_version: i32,
}

#[derive(Debug)]
pub struct InsertReceiptRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct InsertReceiptLineRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct LockPurchaseOrderRow {
    pub status: String,
}

#[derive(Debug)]
pub struct UpdatePurchaseOrderLineRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct ValidateReceiptLineRow {
    pub outcome: Option<String>,
    pub id: Option<wamn_postgres_statements::Uuid>,
}

pub(crate) const FINISH_PURCHASE_ORDER_DIGEST: &str =
    "sha256:d23e50cb5d69842ea6f5bc4d9b3f3511695d07eef02f6386b30f2361328cfd8e";
pub(crate) const INSERT_RECEIPT_DIGEST: &str =
    "sha256:a1d7ff71ad349f0a43e9dad5f65d617f876277e7770409b11c1ad576815e83ab";
pub(crate) const INSERT_RECEIPT_LINE_DIGEST: &str =
    "sha256:46af356f1b2e4640f42ffb8040f14aa7bd88d303f54941cf1d707456337c781d";
pub(crate) const LOCK_PURCHASE_ORDER_DIGEST: &str =
    "sha256:f54302c31a8ac7d1d26fdc1eaa4886f1d60b3656d6850cc5589c1bb2abeb85e2";
pub(crate) const UPDATE_PURCHASE_ORDER_LINE_DIGEST: &str =
    "sha256:1a3515d4c1b24fba54ef76a771a23d00d54c7d745fc868042095a50e5cdff716";
pub(crate) const VALIDATE_RECEIPT_LINE_DIGEST: &str =
    "sha256:32821a6fdbadf9d5946194e95b5f3b4465c44e8412fb40129ae01858a8001f2e";

pub(crate) async fn finish_purchase_order(
    transaction: &mut Transaction,
    purchase_order_id: wamn_postgres_statements::Uuid,
) -> Result<FinishPurchaseOrderRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            FINISH_PURCHASE_ORDER_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(purchase_order_id)],
        )
        .await?;
    wamn_postgres_statements::decode_one(FINISH_PURCHASE_ORDER_DIGEST, rows, |row| {
        Ok(FinishPurchaseOrderRow {
            status: row.decode("status")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn insert_receipt(
    transaction: &mut Transaction,
    purchase_order_id: wamn_postgres_statements::Uuid,
    receipt_reference: String,
    occurred_at: wamn_postgres_statements::TimestampTz,
) -> Result<InsertReceiptRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            INSERT_RECEIPT_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(purchase_order_id),
                wamn_postgres_statements::into_sql_value(receipt_reference),
                wamn_postgres_statements::into_sql_value(occurred_at),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(INSERT_RECEIPT_DIGEST, rows, |row| {
        Ok(InsertReceiptRow {
            id: row.decode("id")?,
        })
    })
}

pub(crate) async fn insert_receipt_line(
    transaction: &mut Transaction,
    receipt_id: wamn_postgres_statements::Uuid,
    line: wamn_postgres_statements::Json,
) -> Result<Vec<InsertReceiptLineRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            INSERT_RECEIPT_LINE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(receipt_id),
                wamn_postgres_statements::into_sql_value(line),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(INSERT_RECEIPT_LINE_DIGEST, rows, |row| {
        Ok(InsertReceiptLineRow {
            id: row.decode("id")?,
        })
    })
}

pub(crate) async fn lock_purchase_order(
    transaction: &mut Transaction,
    purchase_order_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LockPurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            LOCK_PURCHASE_ORDER_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(purchase_order_id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(LOCK_PURCHASE_ORDER_DIGEST, rows, |row| {
        Ok(LockPurchaseOrderRow {
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn update_purchase_order_line(
    transaction: &mut Transaction,
    purchase_order_id: wamn_postgres_statements::Uuid,
    line: wamn_postgres_statements::Json,
) -> Result<Vec<UpdatePurchaseOrderLineRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            UPDATE_PURCHASE_ORDER_LINE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(purchase_order_id),
                wamn_postgres_statements::into_sql_value(line),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(UPDATE_PURCHASE_ORDER_LINE_DIGEST, rows, |row| {
        Ok(UpdatePurchaseOrderLineRow {
            id: row.decode("id")?,
        })
    })
}

pub(crate) async fn validate_receipt_line(
    transaction: &mut Transaction,
    purchase_order_id: wamn_postgres_statements::Uuid,
    line: wamn_postgres_statements::Json,
) -> Result<ValidateReceiptLineRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            VALIDATE_RECEIPT_LINE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(purchase_order_id),
                wamn_postgres_statements::into_sql_value(line),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(VALIDATE_RECEIPT_LINE_DIGEST, rows, |row| {
        Ok(ValidateReceiptLineRow {
            outcome: row.decode("outcome")?,
            id: row.decode("id")?,
        })
    })
}
