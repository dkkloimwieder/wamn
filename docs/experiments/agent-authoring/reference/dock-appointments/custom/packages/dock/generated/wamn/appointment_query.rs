// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct AppointmentQueryRow {
    pub id: wamn_postgres_statements::Uuid,
    pub carrier_id: wamn_postgres_statements::Uuid,
    pub dock_id: wamn_postgres_statements::Uuid,
    pub slot_start: wamn_postgres_statements::TimestampTz,
    pub slot_end: wamn_postgres_statements::TimestampTz,
    pub status: String,
    pub arrived_at: Option<wamn_postgres_statements::TimestampTz>,
}

#[derive(Debug)]
pub(crate) struct LoadDockRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const APPOINTMENT_QUERY_DIGEST: &str = "sha256:894568b31038f8a841e774027267f99859d5a8c5c8b798ffe1094ecb31127f03";
pub(crate) const LOAD_DOCK_DIGEST: &str = "sha256:b030e5364992beb0d7a39dcfaf722e65a094b54793bdbc8043e346d71e756523";

pub(crate) async fn appointment_query(
    transaction: &mut Transaction,
    dock_id: wamn_postgres_statements::Uuid,
    day_start: wamn_postgres_statements::TimestampTz,
    day_end: wamn_postgres_statements::TimestampTz,
    status: String,
) -> Result<Vec<AppointmentQueryRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(APPOINTMENT_QUERY_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(dock_id),
        wamn_postgres_statements::into_sql_value(day_start),
        wamn_postgres_statements::into_sql_value(day_end),
        wamn_postgres_statements::into_sql_value(status),
    ]).await?;
    wamn_postgres_statements::decode_all(APPOINTMENT_QUERY_DIGEST, rows, |row| {
        Ok(AppointmentQueryRow {
            id: row.decode("id")?,
            carrier_id: row.decode("carrier_id")?,
            dock_id: row.decode("dock_id")?,
            slot_start: row.decode("slot_start")?,
            slot_end: row.decode("slot_end")?,
            status: row.decode("status")?,
            arrived_at: row.decode("arrived_at")?,
        })
    })
}

pub(crate) async fn load_dock(
    transaction: &mut Transaction,
    dock_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LoadDockRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(LOAD_DOCK_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(dock_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(LOAD_DOCK_DIGEST, rows, |row| {
        Ok(LoadDockRow {
            id: row.decode("id")?,
        })
    })
}
