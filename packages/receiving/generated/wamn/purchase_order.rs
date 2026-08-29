// @generated from migration IR; do not edit.

use sqlx_core::query_as::query_as;
use wamn_postgres_sqlx::{WamnConnection, WamnPostgres};

#[derive(Debug, sqlx::FromRow)]
pub struct PurchaseOrderRow {
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
    pub created_at: Option<wamn_postgres_sqlx::TimestampTz>,
    pub id: Option<wamn_postgres_sqlx::Uuid>,
    pub purchase_order_number: Option<String>,
    pub row_version: Option<i64>,
    pub status: Option<String>,
    pub supplier_id: Option<wamn_postgres_sqlx::Uuid>,
    pub updated_at: Option<wamn_postgres_sqlx::TimestampTz>,
}

pub(crate) const GET_SQL: &str = include_str!("../sql/purchase_order/get.sql");
pub(crate) const QUERY_0_SQL: &str = include_str!("../../query/open_purchase_order_by_purchase_order_number_ascending.sql");
pub(crate) const QUERY_1_SQL: &str = include_str!("../../query/open_purchase_order_by_purchase_order_number_descending.sql");
pub(crate) const QUERY_2_SQL: &str = include_str!("../../query/open_purchase_order_by_status_ascending.sql");
pub(crate) const QUERY_3_SQL: &str = include_str!("../../query/open_purchase_order_by_status_descending.sql");
pub(crate) const QUERY_4_SQL: &str = include_str!("../../query/open_purchase_order.sql");
pub(crate) const QUERY_5_SQL: &str = include_str!("../../query/open_purchase_order_by_created_at_descending.sql");
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

pub(crate) async fn query_purchase_order_number_ascending(
    connection: &mut WamnConnection,
    supplier_id_filter: Option<wamn_postgres_sqlx::Json>,
    status_filter: Option<wamn_postgres_sqlx::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_sqlx::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, PurchaseOrderRow>(QUERY_0_SQL)
        .bind(supplier_id_filter)
        .bind(status_filter)
        .bind(cursor_key)
        .bind(cursor_id)
        .bind(limit)
        .fetch_all(connection)
        .await
}

pub(crate) async fn query_purchase_order_number_descending(
    connection: &mut WamnConnection,
    supplier_id_filter: Option<wamn_postgres_sqlx::Json>,
    status_filter: Option<wamn_postgres_sqlx::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_sqlx::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, PurchaseOrderRow>(QUERY_1_SQL)
        .bind(supplier_id_filter)
        .bind(status_filter)
        .bind(cursor_key)
        .bind(cursor_id)
        .bind(limit)
        .fetch_all(connection)
        .await
}

pub(crate) async fn query_status_ascending(
    connection: &mut WamnConnection,
    supplier_id_filter: Option<wamn_postgres_sqlx::Json>,
    status_filter: Option<wamn_postgres_sqlx::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_sqlx::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, PurchaseOrderRow>(QUERY_2_SQL)
        .bind(supplier_id_filter)
        .bind(status_filter)
        .bind(cursor_key)
        .bind(cursor_id)
        .bind(limit)
        .fetch_all(connection)
        .await
}

pub(crate) async fn query_status_descending(
    connection: &mut WamnConnection,
    supplier_id_filter: Option<wamn_postgres_sqlx::Json>,
    status_filter: Option<wamn_postgres_sqlx::Json>,
    cursor_key: Option<String>,
    cursor_id: Option<wamn_postgres_sqlx::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, PurchaseOrderRow>(QUERY_3_SQL)
        .bind(supplier_id_filter)
        .bind(status_filter)
        .bind(cursor_key)
        .bind(cursor_id)
        .bind(limit)
        .fetch_all(connection)
        .await
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut WamnConnection,
    supplier_id_filter: Option<wamn_postgres_sqlx::Json>,
    status_filter: Option<wamn_postgres_sqlx::Json>,
    cursor_key: Option<wamn_postgres_sqlx::TimestampTz>,
    cursor_id: Option<wamn_postgres_sqlx::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, PurchaseOrderRow>(QUERY_4_SQL)
        .bind(supplier_id_filter)
        .bind(status_filter)
        .bind(cursor_key)
        .bind(cursor_id)
        .bind(limit)
        .fetch_all(connection)
        .await
}

pub(crate) async fn query_created_at_descending(
    connection: &mut WamnConnection,
    supplier_id_filter: Option<wamn_postgres_sqlx::Json>,
    status_filter: Option<wamn_postgres_sqlx::Json>,
    cursor_key: Option<wamn_postgres_sqlx::TimestampTz>,
    cursor_id: Option<wamn_postgres_sqlx::Uuid>,
    limit: i64,
) -> Result<Vec<PurchaseOrderRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, PurchaseOrderRow>(QUERY_5_SQL)
        .bind(supplier_id_filter)
        .bind(status_filter)
        .bind(cursor_key)
        .bind(cursor_id)
        .bind(limit)
        .fetch_all(connection)
        .await
}

pub(crate) async fn update(
    connection: &mut WamnConnection,
    id: wamn_postgres_sqlx::Uuid,
    expected_row_version: i64,
    supplier_id_present: bool,
    supplier_id_value: Option<wamn_postgres_sqlx::Uuid>,
) -> Result<PurchaseOrderUpdateRow, sqlx_core::error::Error> {
    query_as::<WamnPostgres, PurchaseOrderUpdateRow>(UPDATE_SQL)
        .bind(id)
        .bind(expected_row_version)
        .bind(supplier_id_present)
        .bind(supplier_id_value)
        .fetch_one(connection)
        .await
}
