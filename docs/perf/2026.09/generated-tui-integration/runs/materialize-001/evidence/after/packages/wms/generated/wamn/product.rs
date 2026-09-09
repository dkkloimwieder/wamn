// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct ProductRow {
    pub id: wamn_postgres_statements::Uuid,
    pub product_code: String,
}
