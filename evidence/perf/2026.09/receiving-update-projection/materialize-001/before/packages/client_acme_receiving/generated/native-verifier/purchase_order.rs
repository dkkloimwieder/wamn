// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct PurchaseOrderRow {
    pub acme_inspection_required: bool,
    pub acme_quality_status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub id: uuid::Uuid,
    pub purchase_order_number: String,
    pub row_version: i64,
    pub status: String,
    pub supplier_id: uuid::Uuid,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, sqlx::FromRow)]
pub struct PurchaseOrderUpdateRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i64>,
    pub acme_inspection_required: Option<bool>,
    pub acme_quality_status: Option<String>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub id: Option<uuid::Uuid>,
    pub purchase_order_number: Option<String>,
    pub row_version: Option<i64>,
    pub status: Option<String>,
    pub supplier_id: Option<uuid::Uuid>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

pub(crate) const GET_SQL: &str = include_str!("../sql/purchase_order/get.sql");
pub(crate) const UPDATE_SQL: &str = include_str!("../sql/purchase_order/update.sql");

pub(crate) fn get_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn update_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn update_expected_row_version_bind_fixture() -> i64 {
    0_i64
}
pub(crate) fn update_acme_inspection_required_present_bind_fixture() -> bool {
    false
}
pub(crate) fn update_acme_inspection_required_value_bind_fixture() -> Option<bool> {
    None
}
pub(crate) fn update_acme_quality_status_present_bind_fixture() -> bool {
    false
}
pub(crate) fn update_acme_quality_status_value_bind_fixture() -> Option<String> {
    None
}
