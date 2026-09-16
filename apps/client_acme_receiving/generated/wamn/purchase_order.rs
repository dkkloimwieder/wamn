// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct PurchaseOrderRow {
    pub acme_inspection_required: bool,
    pub acme_quality_status: String,
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub created_by: wamn_postgres_statements::Uuid,
    pub id: wamn_postgres_statements::Uuid,
    pub purchase_order_number: String,
    pub row_version: i64,
    pub status: String,
    pub supplier_id: wamn_postgres_statements::Uuid,
    pub updated_at: wamn_postgres_statements::TimestampTz,
    pub updated_by: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct PurchaseOrderUpdateRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i64>,
    pub acme_inspection_required: Option<bool>,
    pub acme_quality_status: Option<String>,
    pub created_at: Option<wamn_postgres_statements::TimestampTz>,
    pub created_by: Option<wamn_postgres_statements::Uuid>,
    pub id: Option<wamn_postgres_statements::Uuid>,
    pub purchase_order_number: Option<String>,
    pub row_version: Option<i64>,
    pub status: Option<String>,
    pub supplier_id: Option<wamn_postgres_statements::Uuid>,
    pub updated_at: Option<wamn_postgres_statements::TimestampTz>,
    pub updated_by: Option<wamn_postgres_statements::Uuid>,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:39b89be8cfeb1ad9031416d2aac96a6fdcfa092e0f1300f7ed1dfd8799cd415c";
pub(crate) const UPDATE_DIGEST: &str =
    "sha256:09f969f5adda1cbaf63ccc3c6f034e2b44a4a199509cef2085e5a06e14bbbf13";

pub(crate) const UPDATE_UNIQUE_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<PurchaseOrderRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(PurchaseOrderRow {
            acme_inspection_required: row.decode("acme_inspection_required")?,
            acme_quality_status: row.decode("acme_quality_status")?,
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            supplier_id: row.decode("supplier_id")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
        })
    })
}

pub(crate) async fn update(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
    expected_row_version: i64,
    acme_inspection_required_present: bool,
    acme_inspection_required_value: Option<bool>,
    acme_quality_status_present: bool,
    acme_quality_status_value: Option<String>,
) -> Result<PurchaseOrderUpdateRow, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            UPDATE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(expected_row_version),
                wamn_postgres_statements::into_sql_value(acme_inspection_required_present),
                wamn_postgres_statements::into_sql_value(acme_inspection_required_value),
                wamn_postgres_statements::into_sql_value(acme_quality_status_present),
                wamn_postgres_statements::into_sql_value(acme_quality_status_value),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(UPDATE_DIGEST, rows, |row| {
        Ok(PurchaseOrderUpdateRow {
            outcome: row.decode("outcome")?,
            observed_row_version: row.decode("observed_row_version")?,
            acme_inspection_required: row.decode("acme_inspection_required")?,
            acme_quality_status: row.decode("acme_quality_status")?,
            created_at: row.decode("created_at")?,
            created_by: row.decode("created_by")?,
            id: row.decode("id")?,
            purchase_order_number: row.decode("purchase_order_number")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
            supplier_id: row.decode("supplier_id")?,
            updated_at: row.decode("updated_at")?,
            updated_by: row.decode("updated_by")?,
        })
    })
}
