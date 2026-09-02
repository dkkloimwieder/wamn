// @generated from migration IR; do not edit.

use sqlx_core::query_as::query_as;
use wamn_postgres_sqlx::{WamnConnection, WamnPostgres};

#[derive(Debug, sqlx::FromRow)]
pub struct ItemRow {
    pub id: wamn_postgres_sqlx::Uuid,
    pub item_number: String,
}
