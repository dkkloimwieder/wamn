// @generated from migration IR; do not edit.

use sqlx_core::query_as::query_as;
use sqlx_core::transaction::Transaction;
use wamn_postgres_sqlx::WamnPostgres;

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ClaimCommandRow {
    pub receipt_id: wamn_postgres_sqlx::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FinalizeCommandRow {
    pub purchase_order_status: Option<String>,
    pub row_version: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub receipt_id: wamn_postgres_sqlx::Uuid,
    pub purchase_order_id: wamn_postgres_sqlx::Uuid,
    pub purchase_order_status: Option<String>,
    pub row_version: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct FinishPurchaseOrderRow {
    pub status: String,
    pub row_version: i64,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct InsertReceiptRow {
    pub id: wamn_postgres_sqlx::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct InsertReceiptLineRow {
    pub id: wamn_postgres_sqlx::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct LockPurchaseOrderRow {
    pub status: String,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct UpdatePurchaseOrderLineRow {
    pub id: wamn_postgres_sqlx::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct ValidateReceiptLineRow {
    pub outcome: Option<String>,
    pub id: Option<wamn_postgres_sqlx::Uuid>,
}

pub(crate) const CLAIM_COMMAND_SQL: &str = include_str!("../../command/record_receipt/claim_command.sql");
pub(crate) const FINALIZE_COMMAND_SQL: &str = include_str!("../../command/record_receipt/finalize_command.sql");
pub(crate) const FIND_REPLAY_SQL: &str = include_str!("../../command/record_receipt/find_replay.sql");
pub(crate) const FINISH_PURCHASE_ORDER_SQL: &str = include_str!("../../command/record_receipt/finish_purchase_order.sql");
pub(crate) const INSERT_RECEIPT_SQL: &str = include_str!("../../command/record_receipt/insert_receipt.sql");
pub(crate) const INSERT_RECEIPT_LINE_SQL: &str = include_str!("../../command/record_receipt/insert_receipt_line.sql");
pub(crate) const LOCK_PURCHASE_ORDER_SQL: &str = include_str!("../../command/record_receipt/lock_purchase_order.sql");
pub(crate) const UPDATE_PURCHASE_ORDER_LINE_SQL: &str = include_str!("../../command/record_receipt/update_purchase_order_line.sql");
pub(crate) const VALIDATE_RECEIPT_LINE_SQL: &str = include_str!("../../command/record_receipt/validate_receipt_line.sql");

pub(crate) async fn claim_command(
    transaction: &mut Transaction<'_, WamnPostgres>,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    purchase_order_id: wamn_postgres_sqlx::Uuid,
) -> Result<Option<ClaimCommandRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, ClaimCommandRow>(CLAIM_COMMAND_SQL)
        .bind(idempotency_key)
        .bind(canonical_command)
        .bind(purchase_order_id)
        .fetch_optional(&mut **transaction)
        .await
}

pub(crate) async fn finalize_command(
    transaction: &mut Transaction<'_, WamnPostgres>,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    receipt_id: wamn_postgres_sqlx::Uuid,
    purchase_order_status: String,
    row_version: i64,
) -> Result<FinalizeCommandRow, sqlx_core::error::Error> {
    query_as::<WamnPostgres, FinalizeCommandRow>(FINALIZE_COMMAND_SQL)
        .bind(idempotency_key)
        .bind(canonical_command)
        .bind(receipt_id)
        .bind(purchase_order_status)
        .bind(row_version)
        .fetch_one(&mut **transaction)
        .await
}

pub(crate) async fn find_replay(
    transaction: &mut Transaction<'_, WamnPostgres>,
    idempotency_key: String,
) -> Result<Option<FindReplayRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, FindReplayRow>(FIND_REPLAY_SQL)
        .bind(idempotency_key)
        .fetch_optional(&mut **transaction)
        .await
}

pub(crate) async fn finish_purchase_order(
    transaction: &mut Transaction<'_, WamnPostgres>,
    purchase_order_id: wamn_postgres_sqlx::Uuid,
) -> Result<FinishPurchaseOrderRow, sqlx_core::error::Error> {
    query_as::<WamnPostgres, FinishPurchaseOrderRow>(FINISH_PURCHASE_ORDER_SQL)
        .bind(purchase_order_id)
        .fetch_one(&mut **transaction)
        .await
}

pub(crate) async fn insert_receipt(
    transaction: &mut Transaction<'_, WamnPostgres>,
    receipt_id: wamn_postgres_sqlx::Uuid,
    idempotency_key: String,
    purchase_order_id: wamn_postgres_sqlx::Uuid,
    receipt_reference: String,
    occurred_at: wamn_postgres_sqlx::TimestampTz,
) -> Result<InsertReceiptRow, sqlx_core::error::Error> {
    query_as::<WamnPostgres, InsertReceiptRow>(INSERT_RECEIPT_SQL)
        .bind(receipt_id)
        .bind(idempotency_key)
        .bind(purchase_order_id)
        .bind(receipt_reference)
        .bind(occurred_at)
        .fetch_one(&mut **transaction)
        .await
}

pub(crate) async fn insert_receipt_line(
    transaction: &mut Transaction<'_, WamnPostgres>,
    receipt_id: wamn_postgres_sqlx::Uuid,
    line: wamn_postgres_sqlx::Json,
) -> Result<Vec<InsertReceiptLineRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, InsertReceiptLineRow>(INSERT_RECEIPT_LINE_SQL)
        .bind(receipt_id)
        .bind(line)
        .fetch_all(&mut **transaction)
        .await
}

pub(crate) async fn lock_purchase_order(
    transaction: &mut Transaction<'_, WamnPostgres>,
    purchase_order_id: wamn_postgres_sqlx::Uuid,
) -> Result<Option<LockPurchaseOrderRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, LockPurchaseOrderRow>(LOCK_PURCHASE_ORDER_SQL)
        .bind(purchase_order_id)
        .fetch_optional(&mut **transaction)
        .await
}

pub(crate) async fn update_purchase_order_line(
    transaction: &mut Transaction<'_, WamnPostgres>,
    purchase_order_id: wamn_postgres_sqlx::Uuid,
    line: wamn_postgres_sqlx::Json,
) -> Result<Vec<UpdatePurchaseOrderLineRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, UpdatePurchaseOrderLineRow>(UPDATE_PURCHASE_ORDER_LINE_SQL)
        .bind(purchase_order_id)
        .bind(line)
        .fetch_all(&mut **transaction)
        .await
}

pub(crate) async fn validate_receipt_line(
    transaction: &mut Transaction<'_, WamnPostgres>,
    purchase_order_id: wamn_postgres_sqlx::Uuid,
    line: wamn_postgres_sqlx::Json,
) -> Result<ValidateReceiptLineRow, sqlx_core::error::Error> {
    query_as::<WamnPostgres, ValidateReceiptLineRow>(VALIDATE_RECEIPT_LINE_SQL)
        .bind(purchase_order_id)
        .bind(line)
        .fetch_one(&mut **transaction)
        .await
}
