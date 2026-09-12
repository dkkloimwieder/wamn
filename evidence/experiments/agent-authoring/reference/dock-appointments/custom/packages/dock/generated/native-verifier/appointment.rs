// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct AppointmentRow {
    pub arrived_at: Option<chrono::DateTime<chrono::Utc>>,
    pub carrier_id: uuid::Uuid,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub dock_id: uuid::Uuid,
    pub id: uuid::Uuid,
    pub slot_end: chrono::DateTime<chrono::Utc>,
    pub slot_start: chrono::DateTime<chrono::Utc>,
    pub status: String,
}
