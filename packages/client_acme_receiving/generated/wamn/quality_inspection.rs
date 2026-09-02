// @generated from migration IR; do not edit.

use sqlx_core::query_as::query_as;
use wamn_postgres_sqlx::{WamnConnection, WamnPostgres};

#[derive(Debug, sqlx::FromRow)]
pub struct QualityInspectionRow {
    pub receipt_id: wamn_postgres_sqlx::Uuid,
    pub row_version: i64,
    pub status: String,
}
