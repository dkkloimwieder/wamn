//! Wire adapters for Acme Receiving operations backed by generated SQL.

use wamn_postgres_statements::{Connection, TransactionView, Uuid as WamnUuid};

use crate::error::AccessError;
use crate::statements::{
    quality_approve_inspection as approve_sql, quality_create_inspection as create_sql,
    quality_load_purchase_order_detail as detail_sql,
    receiving_record_receipt_participant as receipt_participant_sql,
};

/// Classify failure to acquire the host-issued participant transaction view.
pub fn participant_view_error(source: &wamn_postgres_statements::StatementError) -> AccessError {
    AccessError::from_statement("acquire participant transaction view", source)
}

/// Result of one successful purchase-order detail load.
#[derive(Debug)]
pub struct PurchaseOrderDetailValue {
    pub id: WamnUuid,
    pub purchase_order_number: String,
    pub supplier_id: WamnUuid,
    pub status: String,
    pub row_version: i32,
    pub acme_inspection_required: bool,
    pub acme_quality_status: String,
}

impl From<detail_sql::LoadPurchaseOrderDetailRow> for PurchaseOrderDetailValue {
    fn from(row: detail_sql::LoadPurchaseOrderDetailRow) -> Self {
        Self {
            id: row.id,
            purchase_order_number: row.purchase_order_number,
            supplier_id: row.supplier_id,
            status: row.status,
            row_version: row.row_version,
            acme_inspection_required: row.acme_inspection_required,
            acme_quality_status: row.acme_quality_status,
        }
    }
}

/// Result of one successful inspection approval.
#[derive(Debug)]
pub struct ApproveInspectionValue {
    pub receipt_id: WamnUuid,
    pub status: String,
    pub row_version: i32,
    pub purchase_order_id: WamnUuid,
    pub purchase_order_row_version: i32,
}

/// Parse any accepted UUID spelling and re-spell it lowercase-hyphenated.
///
/// Case is representation, so an input arrives in any spelling and reaches the
/// database in one. The idempotency key hashes the re-spelled bytes, never the
/// arriving bytes, so a caller whose retry infrastructure re-spells a UUID gets
/// one command instead of an idempotency conflict.
fn parse_uuid(value: &str, field: &'static str) -> Result<WamnUuid, AccessError> {
    uuid::Uuid::parse_str(value)
        .map(|parsed| WamnUuid(parsed.hyphenated().to_string()))
        .map_err(|_| AccessError::invalid("input is not a UUID", field))
}

fn approve_inspection_value(
    row: approve_sql::ApproveInspectionRow,
    expected_row_version: i32,
) -> Result<ApproveInspectionValue, AccessError> {
    match row.outcome.as_deref() {
        Some("not_found") => Err(AccessError::not_found("quality_inspection does not exist")),
        Some("concurrency_conflict") => row.observed_row_version.map_or_else(
            || {
                Err(AccessError::internal(
                    "quality_inspection concurrency refusal omitted observed_row_version",
                ))
            },
            |observed| {
                Err(AccessError::concurrency_conflict(
                    format!(
                        "quality_inspection row_version {observed} does not match {expected_row_version}"
                    ),
                    observed,
                ))
            },
        ),
        Some("approved") => match (
            row.receipt_id,
            row.status,
            row.row_version,
            row.purchase_order_id,
            row.purchase_order_row_version,
        ) {
            (
                Some(receipt_id),
                Some(status),
                Some(row_version),
                Some(purchase_order_id),
                Some(purchase_order_row_version),
            ) if status == "approved" => Ok(ApproveInspectionValue {
                receipt_id,
                status,
                row_version,
                purchase_order_id,
                purchase_order_row_version,
            }),
            _ => Err(AccessError::internal(
                "quality_inspection approval returned an incomplete row",
            )),
        },
        _ => Err(AccessError::internal(
            "quality.approve_inspection returned a null or unknown outcome",
        )),
    }
}

/// Execute `quality.load_purchase_order_detail` against its verified projection.
pub async fn quality_load_purchase_order_detail(
    connection: &mut Connection,
    purchase_order_id: &str,
) -> Result<PurchaseOrderDetailValue, AccessError> {
    let id = parse_uuid(purchase_order_id, "purchase_order_id")?;
    let mut transaction = connection
        .begin()
        .await
        .map_err(|source| AccessError::from_statement("begin purchase_order detail", &source))?;
    let row = detail_sql::load_purchase_order_detail(&mut transaction, id)
        .await
        .map_err(|source| AccessError::from_statement("load purchase_order detail", &source))?;
    transaction
        .commit()
        .await
        .map_err(|source| AccessError::from_statement("commit purchase_order detail", &source))?;
    row.map(Into::into)
        .ok_or_else(|| AccessError::not_found("purchase_order does not exist"))
}

/// Execute `quality.approve_inspection` as one transaction per input item.
pub async fn quality_approve_inspection(
    connection: &mut Connection,
    receipt_id: &str,
    expected_row_version: i32,
) -> Result<ApproveInspectionValue, AccessError> {
    let receipt_id = parse_uuid(receipt_id, "receipt_id")?;
    let mut transaction = connection
        .begin()
        .await
        .map_err(|source| AccessError::from_statement("begin inspection approval", &source))?;
    let row = approve_sql::approve_inspection(&mut transaction, receipt_id, expected_row_version)
        .await
        .map_err(|source| AccessError::from_statement("approve quality_inspection", &source))?;
    let value = approve_inspection_value(row, expected_row_version)?;
    transaction
        .commit()
        .await
        .map_err(|source| AccessError::from_statement("commit inspection approval", &source))?;
    Ok(value)
}

/// Execute private `quality.create_inspection` without caller or permission synthesis.
pub async fn quality_create_inspection(event: &str, receipt_id: &str) -> Result<(), AccessError> {
    if event != "insert" {
        return Err(AccessError::invalid(
            "event input does not match its contract",
            "event",
        ));
    }
    let receipt_id = parse_uuid(receipt_id, "new.id")?;
    let mut connection = Connection::new();
    let mut transaction = connection
        .begin()
        .await
        .map_err(|source| AccessError::from_statement("begin inspection creation", &source))?;
    let inserted = create_sql::insert_inspection(&mut transaction, receipt_id.clone())
        .await
        .map_err(|source| AccessError::from_statement("insert quality_inspection", &source))?;
    let persisted_id = match inserted {
        Some(row) => Some(row.receipt_id),
        None => create_sql::load_inspection(&mut transaction, receipt_id.clone())
            .await
            .map_err(|source| {
                AccessError::from_statement("load quality_inspection replay", &source)
            })?
            .map(|row| row.receipt_id),
    };
    if persisted_id.as_ref().is_some_and(|id| id != &receipt_id) {
        return Err(AccessError::internal(
            "quality_inspection returned a different receipt id",
        ));
    }
    transaction
        .commit()
        .await
        .map_err(|source| AccessError::from_statement("commit inspection creation", &source))?;
    Ok(())
}

/// Apply Acme quality control inside the selected base receipt transaction.
pub async fn record_receipt_participant(
    transaction: &mut TransactionView,
    receipt_id: &str,
    purchase_order_id: &str,
) -> Result<(), AccessError> {
    let receipt_id = parse_uuid(receipt_id, "receipt_id")?;
    let purchase_order_id = parse_uuid(purchase_order_id, "purchase_order_id")?;
    let row = receipt_participant_sql::record_receipt_participant(
        transaction,
        receipt_id.clone(),
        purchase_order_id,
    )
    .await
    .map_err(|source| AccessError::from_statement("apply receipt quality control", &source))?;
    match row.outcome.as_deref() {
        Some("not_required") => Ok(()),
        Some("approved") if row.receipt_id.as_ref() == Some(&receipt_id) => Ok(()),
        Some("quality_not_approved") => Err(AccessError::invalid(
            "purchase_order quality control is required and is not approved",
            "purchase_order_id",
        )),
        _ => Err(AccessError::internal(
            "receipt participant returned an incomplete or unknown outcome",
        )),
    }
}
