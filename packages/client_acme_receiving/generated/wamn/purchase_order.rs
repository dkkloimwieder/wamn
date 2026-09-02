// @generated from migration IR; do not edit.

use sqlx_core::query_as::query_as;
use wamn_postgres_sqlx::{WamnConnection, WamnPostgres};

#[derive(Debug, sqlx::FromRow)]
pub struct PurchaseOrderRow {
    pub acme_inspection_required: bool,
    pub acme_quality_status: String,
    pub created_at: wamn_postgres_sqlx::TimestampTz,
    pub id: wamn_postgres_sqlx::Uuid,
    pub purchase_order_number: String,
    pub row_version: i64,
    pub status: String,
    pub supplier_id: wamn_postgres_sqlx::Uuid,
    pub updated_at: wamn_postgres_sqlx::TimestampTz,
}

#[derive(Debug, sqlx::FromRow)]
pub struct PurchaseOrderUpdateRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i64>,
    pub acme_inspection_required: Option<bool>,
    pub acme_quality_status: Option<String>,
    pub created_at: Option<wamn_postgres_sqlx::TimestampTz>,
    pub id: Option<wamn_postgres_sqlx::Uuid>,
    pub purchase_order_number: Option<String>,
    pub row_version: Option<i64>,
    pub status: Option<String>,
    pub supplier_id: Option<wamn_postgres_sqlx::Uuid>,
    pub updated_at: Option<wamn_postgres_sqlx::TimestampTz>,
}

pub(crate) const GET_SQL: &str = include_str!("../sql/purchase_order/get.sql");
pub(crate) const UPDATE_SQL: &str = include_str!("../sql/purchase_order/update.sql");

pub(crate) const UPDATE_UNIQUE_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_CHECK_CONSTRAINTS: &[&str] = &[];

pub(crate) async fn get(
    connection: &mut WamnConnection,
    id: wamn_postgres_sqlx::Uuid,
) -> Result<Option<PurchaseOrderRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, PurchaseOrderRow>(GET_SQL)
        .bind(id)
        .fetch_optional(connection)
        .await
}

pub(crate) async fn update(
    connection: &mut WamnConnection,
    id: wamn_postgres_sqlx::Uuid,
    expected_row_version: i64,
    acme_inspection_required_present: bool,
    acme_inspection_required_value: Option<bool>,
    acme_quality_status_present: bool,
    acme_quality_status_value: Option<String>,
) -> Result<PurchaseOrderUpdateRow, sqlx_core::error::Error> {
    query_as::<WamnPostgres, PurchaseOrderUpdateRow>(UPDATE_SQL)
        .bind(id)
        .bind(expected_row_version)
        .bind(acme_inspection_required_present)
        .bind(acme_inspection_required_value)
        .bind(acme_quality_status_present)
        .bind(acme_quality_status_value)
        .fetch_one(connection)
        .await
}
