// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct AppointmentRow {
    pub arrived_at: Option<wamn_postgres_statements::TimestampTz>,
    pub carrier_id: wamn_postgres_statements::Uuid,
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub dock_id: wamn_postgres_statements::Uuid,
    pub id: wamn_postgres_statements::Uuid,
    pub slot_end: wamn_postgres_statements::TimestampTz,
    pub slot_start: wamn_postgres_statements::TimestampTz,
    pub status: String,
}
