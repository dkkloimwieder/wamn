// @generated from the package manifest and migration IR; do not edit.

// The generated `purchase_order` operations.

#[allow(unused_imports)]
use wamn_data_access::{Direction, Invalid, Page, cursor, scalar};
use wamn_postgres_statements::Connection;

#[allow(unused_imports)]
use super::error::{Constraints, Error};

/// The statement accessors of the model.
pub mod sql {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../generated/wamn/purchase_order.rs"
    ));
}

pub use sql::PurchaseOrderRow;
pub use sql::PurchaseOrderUpdateRow;

const GET_OPERATION: &str = "purchase_order.get";
const UPDATE_OPERATION: &str = "purchase_order.update";

/// `purchase_order.get`: load one row by id.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
pub async fn get(connection: &mut Connection, id: &str) -> Result<PurchaseOrderRow, Error> {
    let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;
    sql::get(connection, id.clone())
        .await
        .map_err(|error| Error::from_statement(&error, &Constraints::NONE, GET_OPERATION))?
        .ok_or_else(|| Error::not_found(&id))
}

/// `purchase_order.update`: change one row at the revision the caller last read.
///
/// # Errors
///
/// [`Error`] carrying the literal the operation contract declares.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    connection: &mut Connection,
    id: &str,
    expected_row_version: i32,
    acme_inspection_required: Option<Option<bool>>,
    acme_quality_status: Option<Option<String>>,
) -> Result<PurchaseOrderRow, Error> {
    let id = scalar::uuid(id).map_err(|Invalid| Error::invalid("id"))?;
    if matches!(acme_inspection_required, Some(None)) {
        return Err(Error::invalid("change.acme_inspection_required"));
    }
    let acme_inspection_required_present = acme_inspection_required.is_some();
    let acme_inspection_required = acme_inspection_required.flatten();
    if matches!(acme_quality_status, Some(None)) {
        return Err(Error::invalid("change.acme_quality_status"));
    }
    let acme_quality_status_present = acme_quality_status.is_some();
    let acme_quality_status = acme_quality_status.flatten();
    let row = sql::update(
        connection,
        id.clone(),
        expected_row_version,
        acme_inspection_required_present,
        acme_inspection_required,
        acme_quality_status_present,
        acme_quality_status,
    )
    .await
    .map_err(|error| {
        Error::from_statement(
            &error,
            &Constraints {
                unique: sql::UPDATE_UNIQUE_CONSTRAINTS,
                foreign_key: sql::UPDATE_FOREIGN_KEY_CONSTRAINTS,
                check: sql::UPDATE_CHECK_CONSTRAINTS,
                exclusion: sql::UPDATE_EXCLUSION_CONSTRAINTS,
            },
            UPDATE_OPERATION,
        )
    })?;
    match row.outcome.as_deref() {
        Some("updated") => Ok(PurchaseOrderRow {
            acme_inspection_required: row.acme_inspection_required.ok_or_else(Error::internal)?,
            acme_quality_status: row.acme_quality_status.ok_or_else(Error::internal)?,
            created_at: row.created_at.ok_or_else(Error::internal)?,
            created_by: row.created_by.ok_or_else(Error::internal)?,
            id: row.id.ok_or_else(Error::internal)?,
            purchase_order_number: row.purchase_order_number.ok_or_else(Error::internal)?,
            row_version: row.row_version.ok_or_else(Error::internal)?,
            status: row.status.ok_or_else(Error::internal)?,
            supplier_id: row.supplier_id.ok_or_else(Error::internal)?,
            updated_at: row.updated_at.ok_or_else(Error::internal)?,
            updated_by: row.updated_by.ok_or_else(Error::internal)?,
        }),
        Some("not_found") => Err(Error::not_found(&id)),
        Some("concurrency_conflict") => Err(Error::conflict(
            i64::from(expected_row_version),
            i64::from(row.observed_row_version.ok_or_else(Error::internal)?),
        )),
        _ => Err(Error::internal()),
    }
}
