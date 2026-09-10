// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct ItemRow {
    pub id: wamn_postgres_statements::Uuid,
    pub item_number: String,
}
