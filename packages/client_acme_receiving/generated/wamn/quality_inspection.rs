// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct QualityInspectionRow {
    pub receipt_id: wamn_postgres_statements::Uuid,
    pub row_version: i64,
    pub status: String,
}
