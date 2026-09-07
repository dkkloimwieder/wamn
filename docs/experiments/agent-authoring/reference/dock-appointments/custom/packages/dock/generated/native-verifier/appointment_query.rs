// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct AppointmentQueryRow {
    pub id: uuid::Uuid,
    pub carrier_id: uuid::Uuid,
    pub dock_id: uuid::Uuid,
    pub slot_start: chrono::DateTime<chrono::Utc>,
    pub slot_end: chrono::DateTime<chrono::Utc>,
    pub status: String,
    pub arrived_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct LoadDockRow {
    pub id: uuid::Uuid,
}

pub(crate) const APPOINTMENT_QUERY_SQL: &str = include_str!("../../query/appointment_query.sql");
pub(crate) const LOAD_DOCK_SQL: &str = include_str!("../../query/load_dock.sql");

pub(crate) fn appointment_query_dock_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn appointment_query_day_start_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn appointment_query_day_end_bind_fixture() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::UNIX_EPOCH
}
pub(crate) fn appointment_query_status_bind_fixture() -> String {
    String::new()
}
pub(crate) fn load_dock_dock_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
