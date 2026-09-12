// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct ClaimCommandRow {
    pub appointment_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct FinalizeCommandRow {
    pub status: Option<String>,
}

#[derive(Debug)]
pub(crate) struct FindOverlapRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub appointment_id: wamn_postgres_statements::Uuid,
    pub status: Option<String>,
}

#[derive(Debug)]
pub(crate) struct InsertAppointmentRow {
    pub id: wamn_postgres_statements::Uuid,
    pub status: String,
}

#[derive(Debug)]
pub(crate) struct LoadCarrierRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct LockDockRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const CLAIM_COMMAND_DIGEST: &str = "sha256:c22af2c19e1b4a7d78c5c2324c8474e74dd9f5757cd005bcc13a5de2d9eb47c9";
pub(crate) const FINALIZE_COMMAND_DIGEST: &str = "sha256:e2747fcd7bd2d2228fa177c6cfbc6acd2b0c20b7d09ee6c8a2994c3e053c5512";
pub(crate) const FIND_OVERLAP_DIGEST: &str = "sha256:fdd0eedac7233332adf78ae284a49b9b94ca9f0e5b8cb23b54adb56b4ad1dc27";
pub(crate) const FIND_REPLAY_DIGEST: &str = "sha256:591b1d071a53940682cf2034559facdfe3f5acd721ea14fa1cd21b89b213b078";
pub(crate) const INSERT_APPOINTMENT_DIGEST: &str = "sha256:0a42c22ca904dc9bf3c4448769ffab50e1e2e74d8e7acf61f354a78cfe544630";
pub(crate) const LOAD_CARRIER_DIGEST: &str = "sha256:e8b5ee92d485b0ddd803646c6997aca68e0a7cde2409a6781e2cec76870079b1";
pub(crate) const LOCK_DOCK_DIGEST: &str = "sha256:22388fb8416f6d186429d2b2467214773cb3aba980e2b914a133d8b4df8067f4";

pub(crate) async fn claim_command(
    transaction: &mut Transaction,
    idempotency_key: String,
    canonical_command: Vec<u8>,
) -> Result<Option<ClaimCommandRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(CLAIM_COMMAND_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(canonical_command),
    ]).await?;
    wamn_postgres_statements::decode_optional(CLAIM_COMMAND_DIGEST, rows, |row| {
        Ok(ClaimCommandRow {
            appointment_id: row.decode("appointment_id")?,
        })
    })
}

pub(crate) async fn finalize_command(
    transaction: &mut Transaction,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    appointment_id: wamn_postgres_statements::Uuid,
    status: String,
) -> Result<FinalizeCommandRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(FINALIZE_COMMAND_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(canonical_command),
        wamn_postgres_statements::into_sql_value(appointment_id),
        wamn_postgres_statements::into_sql_value(status),
    ]).await?;
    wamn_postgres_statements::decode_one(FINALIZE_COMMAND_DIGEST, rows, |row| {
        Ok(FinalizeCommandRow {
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn find_overlap(
    transaction: &mut Transaction,
    dock_id: wamn_postgres_statements::Uuid,
    slot_start: wamn_postgres_statements::TimestampTz,
    slot_end: wamn_postgres_statements::TimestampTz,
) -> Result<Option<FindOverlapRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(FIND_OVERLAP_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(dock_id),
        wamn_postgres_statements::into_sql_value(slot_start),
        wamn_postgres_statements::into_sql_value(slot_end),
    ]).await?;
    wamn_postgres_statements::decode_optional(FIND_OVERLAP_DIGEST, rows, |row| {
        Ok(FindOverlapRow {
            id: row.decode("id")?,
        })
    })
}

pub(crate) async fn find_replay(
    transaction: &mut Transaction,
    idempotency_key: String,
) -> Result<Option<FindReplayRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(FIND_REPLAY_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
    ]).await?;
    wamn_postgres_statements::decode_optional(FIND_REPLAY_DIGEST, rows, |row| {
        Ok(FindReplayRow {
            canonical_command: row.decode("canonical_command")?,
            appointment_id: row.decode("appointment_id")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn insert_appointment(
    transaction: &mut Transaction,
    appointment_id: wamn_postgres_statements::Uuid,
    carrier_id: wamn_postgres_statements::Uuid,
    dock_id: wamn_postgres_statements::Uuid,
    slot_start: wamn_postgres_statements::TimestampTz,
    slot_end: wamn_postgres_statements::TimestampTz,
) -> Result<InsertAppointmentRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(INSERT_APPOINTMENT_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(appointment_id),
        wamn_postgres_statements::into_sql_value(carrier_id),
        wamn_postgres_statements::into_sql_value(dock_id),
        wamn_postgres_statements::into_sql_value(slot_start),
        wamn_postgres_statements::into_sql_value(slot_end),
    ]).await?;
    wamn_postgres_statements::decode_one(INSERT_APPOINTMENT_DIGEST, rows, |row| {
        Ok(InsertAppointmentRow {
            id: row.decode("id")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn load_carrier(
    transaction: &mut Transaction,
    carrier_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LoadCarrierRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(LOAD_CARRIER_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(carrier_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(LOAD_CARRIER_DIGEST, rows, |row| {
        Ok(LoadCarrierRow {
            id: row.decode("id")?,
        })
    })
}

pub(crate) async fn lock_dock(
    transaction: &mut Transaction,
    dock_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LockDockRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(LOCK_DOCK_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(dock_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(LOCK_DOCK_DIGEST, rows, |row| {
        Ok(LockDockRow {
            id: row.decode("id")?,
        })
    })
}
