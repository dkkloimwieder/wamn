//! Wire adapters for Acme Receiving operations backed by generated SQL.

use wamn_postgres_statements::{Connection, TransactionView, Uuid as WamnUuid};

use crate::error::{AccessError, AllowedConstraints};
use crate::generated::{
    purchase_order as purchase_order_sql, quality_approve_inspection as approve_sql,
    quality_create_inspection as create_sql, quality_load_purchase_order_detail as detail_sql,
    receiving_record_receipt_participant as receipt_participant_sql,
};

pub use crate::generated::purchase_order::PurchaseOrderRow;

/// Classify failure to acquire the host-issued participant transaction view.
pub fn participant_view_error(source: &wamn_postgres_statements::StatementError) -> AccessError {
    AccessError::from_statement("acquire participant transaction view", source)
}

const UPDATE_CONSTRAINTS: AllowedConstraints = AllowedConstraints {
    exclusion: purchase_order_sql::UPDATE_EXCLUSION_CONSTRAINTS,
};

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

#[expect(
    clippy::option_option,
    reason = "WIT update fields distinguish absent, explicit null, and value"
)]
fn update_value<T>(
    value: Option<Option<T>>,
    field: &'static str,
) -> Result<(bool, Option<T>), AccessError> {
    match value {
        None => Ok((false, None)),
        Some(None) => Err(AccessError::invalid(
            "non-null field does not accept explicit null",
            field,
        )),
        Some(Some(value)) => Ok((true, Some(value))),
    }
}

fn quality_status(value: String) -> Result<String, AccessError> {
    if matches!(value.as_str(), "not_required" | "pending" | "approved") {
        Ok(value)
    } else {
        Err(AccessError::invalid(
            "acme_quality_status is outside the closed vocabulary",
            "change.acme_quality_status",
        ))
    }
}

fn purchase_order_update_value(
    row: purchase_order_sql::PurchaseOrderUpdateRow,
    expected_row_version: i32,
) -> Result<PurchaseOrderRow, AccessError> {
    match row.outcome.as_deref() {
        Some("not_found") => Err(AccessError::not_found("purchase_order does not exist")),
        Some("concurrency_conflict") => row.observed_row_version.map_or_else(
            || {
                Err(AccessError::internal(
                    "purchase_order concurrency refusal omitted observed_row_version",
                ))
            },
            |observed| {
                Err(AccessError::concurrency_conflict(
                    format!(
                        "purchase_order row_version {observed} does not match {expected_row_version}"
                    ),
                    observed,
                ))
            },
        ),
        Some("updated") => match (
            row.id,
            row.purchase_order_number,
            row.supplier_id,
            row.status,
            row.row_version,
            row.created_at,
            row.created_by,
            row.updated_at,
            row.updated_by,
            row.acme_inspection_required,
            row.acme_quality_status,
        ) {
            (
                Some(id),
                Some(purchase_order_number),
                Some(supplier_id),
                Some(status),
                Some(row_version),
                Some(created_at),
                Some(created_by),
                Some(updated_at),
                Some(updated_by),
                Some(acme_inspection_required),
                Some(acme_quality_status),
            ) => Ok(PurchaseOrderRow {
                acme_inspection_required,
                acme_quality_status,
                created_at,
                created_by,
                id,
                purchase_order_number,
                row_version,
                status,
                supplier_id,
                updated_at,
                updated_by,
            }),
            _ => Err(AccessError::internal(
                "purchase_order update returned an incomplete row",
            )),
        },
        _ => Err(AccessError::internal(
            "purchase_order.update returned a null or unknown outcome",
        )),
    }
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

/// Execute one typed `purchase_order.get` against generated Acme SQL.
pub async fn purchase_order_get(
    connection: &mut Connection,
    id: &str,
) -> Result<PurchaseOrderRow, AccessError> {
    let id = parse_uuid(id, "id")?;
    purchase_order_sql::get(connection, id)
        .await
        .map_err(|source| AccessError::from_statement("load purchase_order", &source))?
        .ok_or_else(|| AccessError::not_found("purchase_order does not exist"))
}

/// Execute one typed `purchase_order.update` against generated Acme SQL.
pub async fn purchase_order_update(
    connection: &mut Connection,
    id: &str,
    expected_row_version: i32,
    acme_inspection_required: Option<Option<bool>>,
    acme_quality_status: Option<Option<String>>,
) -> Result<PurchaseOrderRow, AccessError> {
    let id = parse_uuid(id, "id")?;
    let (inspection_present, inspection_value) =
        update_value(acme_inspection_required, "change.acme_inspection_required")?;
    let (quality_present, quality_value) =
        update_value(acme_quality_status, "change.acme_quality_status")?;
    let quality_value = quality_value.map(quality_status).transpose()?;
    let row = purchase_order_sql::update(
        connection,
        id,
        expected_row_version,
        inspection_present,
        inspection_value,
        quality_present,
        quality_value,
    )
    .await
    .map_err(|source| {
        AccessError::from_statement_with_constraints(
            "update purchase_order",
            &source,
            UPDATE_CONSTRAINTS,
        )
    })?;
    purchase_order_update_value(row, expected_row_version)
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
