// @generated from migration IR; do not edit.

use sqlx_core::query_as::query_as;
use sqlx_core::transaction::Transaction;
use wamn_postgres_sqlx::WamnPostgres;

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct InsertInspectionRow {
    pub receipt_id: wamn_postgres_sqlx::Uuid,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct LoadInspectionRow {
    pub receipt_id: wamn_postgres_sqlx::Uuid,
}

pub(crate) const INSERT_INSPECTION_SQL: &str = include_str!("../../command/create_inspection/insert_inspection.sql");
pub(crate) const LOAD_INSPECTION_SQL: &str = include_str!("../../command/create_inspection/load_inspection.sql");

pub(crate) async fn insert_inspection(
    transaction: &mut Transaction<'_, WamnPostgres>,
    receipt_id: wamn_postgres_sqlx::Uuid,
) -> Result<Option<InsertInspectionRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, InsertInspectionRow>(INSERT_INSPECTION_SQL)
        .bind(receipt_id)
        .fetch_optional(&mut **transaction)
        .await
}

pub(crate) async fn load_inspection(
    transaction: &mut Transaction<'_, WamnPostgres>,
    receipt_id: wamn_postgres_sqlx::Uuid,
) -> Result<Option<LoadInspectionRow>, sqlx_core::error::Error> {
    query_as::<WamnPostgres, LoadInspectionRow>(LOAD_INSPECTION_SQL)
        .bind(receipt_id)
        .fetch_optional(&mut **transaction)
        .await
}
