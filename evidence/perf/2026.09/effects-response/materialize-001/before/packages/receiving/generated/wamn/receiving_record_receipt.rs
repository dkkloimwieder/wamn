// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct ClaimCommandRow {
    pub receipt_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct FinalizeCommandRow {
    pub purchase_order_status: Option<String>,
    pub row_version: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub receipt_id: wamn_postgres_statements::Uuid,
    pub purchase_order_id: wamn_postgres_statements::Uuid,
    pub purchase_order_status: Option<String>,
    pub row_version: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct FinishPurchaseOrderRow {
    pub status: String,
    pub row_version: i64,
}

#[derive(Debug)]
pub(crate) struct InsertReceiptRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct InsertReceiptLineRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct LockPurchaseOrderRow {
    pub status: String,
}

#[derive(Debug)]
pub(crate) struct UpdatePurchaseOrderLineRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct ValidateReceiptLineRow {
    pub outcome: Option<String>,
    pub id: Option<wamn_postgres_statements::Uuid>,
}

pub(crate) const CLAIM_COMMAND_DIGEST: &str = "sha256:6b854d40de1d42cab4ab852d0d6b1b6d869e7d5117a0dbd9c7f00b3602874430";
pub(crate) const FINALIZE_COMMAND_DIGEST: &str = "sha256:a5bb7392dff080e683c8fad4a3abd813bcd8db55230ff2a826552c9f8cb5fd15";
pub(crate) const FIND_REPLAY_DIGEST: &str = "sha256:21136f24dadfdf9f5bf9f17b25beb189f50cca409acae2ff3ea97c7997107025";
pub(crate) const FINISH_PURCHASE_ORDER_DIGEST: &str = "sha256:2add12d3a2d7bd9fe80df600e7c46e77c0b55f7213b75131a5e3ef4473004b36";
pub(crate) const INSERT_RECEIPT_DIGEST: &str = "sha256:6ee94d218f33fda99dcc91f7b72265119b16db159624597c9e5d584d7a35a7b0";
pub(crate) const INSERT_RECEIPT_LINE_DIGEST: &str = "sha256:46af356f1b2e4640f42ffb8040f14aa7bd88d303f54941cf1d707456337c781d";
pub(crate) const LOCK_PURCHASE_ORDER_DIGEST: &str = "sha256:f54302c31a8ac7d1d26fdc1eaa4886f1d60b3656d6850cc5589c1bb2abeb85e2";
pub(crate) const UPDATE_PURCHASE_ORDER_LINE_DIGEST: &str = "sha256:1a3515d4c1b24fba54ef76a771a23d00d54c7d745fc868042095a50e5cdff716";
pub(crate) const VALIDATE_RECEIPT_LINE_DIGEST: &str = "sha256:32821a6fdbadf9d5946194e95b5f3b4465c44e8412fb40129ae01858a8001f2e";

pub(crate) async fn claim_command(
    transaction: &mut Transaction,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    purchase_order_id: wamn_postgres_statements::Uuid,
) -> Result<Option<ClaimCommandRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(CLAIM_COMMAND_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(canonical_command),
        wamn_postgres_statements::into_sql_value(purchase_order_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(CLAIM_COMMAND_DIGEST, rows, |row| {
        Ok(ClaimCommandRow {
            receipt_id: row.decode("receipt_id")?,
        })
    })
}

pub(crate) async fn finalize_command(
    transaction: &mut Transaction,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    receipt_id: wamn_postgres_statements::Uuid,
    purchase_order_status: String,
    row_version: i64,
) -> Result<FinalizeCommandRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(FINALIZE_COMMAND_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(canonical_command),
        wamn_postgres_statements::into_sql_value(receipt_id),
        wamn_postgres_statements::into_sql_value(purchase_order_status),
        wamn_postgres_statements::into_sql_value(row_version),
    ]).await?;
    wamn_postgres_statements::decode_one(FINALIZE_COMMAND_DIGEST, rows, |row| {
        Ok(FinalizeCommandRow {
            purchase_order_status: row.decode("purchase_order_status")?,
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
            receipt_id: row.decode("receipt_id")?,
            purchase_order_id: row.decode("purchase_order_id")?,
            purchase_order_status: row.decode("purchase_order_status")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn finish_purchase_order(
    transaction: &mut Transaction,
    purchase_order_id: wamn_postgres_statements::Uuid,
) -> Result<FinishPurchaseOrderRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(FINISH_PURCHASE_ORDER_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(purchase_order_id),
    ]).await?;
    wamn_postgres_statements::decode_one(FINISH_PURCHASE_ORDER_DIGEST, rows, |row| {
        Ok(FinishPurchaseOrderRow {
            status: row.decode("status")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn insert_receipt(
    transaction: &mut Transaction,
    receipt_id: wamn_postgres_statements::Uuid,
    idempotency_key: String,
    purchase_order_id: wamn_postgres_statements::Uuid,
    receipt_reference: String,
    occurred_at: wamn_postgres_statements::TimestampTz,
) -> Result<InsertReceiptRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(INSERT_RECEIPT_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(receipt_id),
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(purchase_order_id),
        wamn_postgres_statements::into_sql_value(receipt_reference),
        wamn_postgres_statements::into_sql_value(occurred_at),
    ]).await?;
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
    let rows = transaction.run(INSERT_RECEIPT_LINE_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(receipt_id),
        wamn_postgres_statements::into_sql_value(line),
    ]).await?;
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
    let rows = transaction.run(LOCK_PURCHASE_ORDER_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(purchase_order_id),
    ]).await?;
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
    let rows = transaction.run(UPDATE_PURCHASE_ORDER_LINE_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(purchase_order_id),
        wamn_postgres_statements::into_sql_value(line),
    ]).await?;
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
    let rows = transaction.run(VALIDATE_RECEIPT_LINE_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(purchase_order_id),
        wamn_postgres_statements::into_sql_value(line),
    ]).await?;
    wamn_postgres_statements::decode_one(VALIDATE_RECEIPT_LINE_DIGEST, rows, |row| {
        Ok(ValidateReceiptLineRow {
            outcome: row.decode("outcome")?,
            id: row.decode("id")?,
        })
    })
}
