// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct LocationRow {
    pub id: wamn_postgres_statements::Uuid,
    pub location_code: String,
}
