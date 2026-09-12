// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct ClaimCommandRow {
    pub check_in_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct FinalizeCommandRow {
    pub status: Option<String>,
    pub arrived_at: Option<wamn_postgres_statements::TimestampTz>,
}

#[derive(Debug)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub check_in_id: wamn_postgres_statements::Uuid,
    pub status: Option<String>,
    pub arrived_at: Option<wamn_postgres_statements::TimestampTz>,
}

#[derive(Debug)]
pub(crate) struct LockAppointmentRow {
    pub id: wamn_postgres_statements::Uuid,
    pub status: String,
}

#[derive(Debug)]
pub(crate) struct RecordArrivalRow {
    pub id: wamn_postgres_statements::Uuid,
    pub status: String,
    pub arrived_at: wamn_postgres_statements::TimestampTz,
}

pub(crate) const CLAIM_COMMAND_DIGEST: &str = "sha256:995860195f7a6c049f6ea1a60d17fd4b1f5f3dda42bc4affb1309fcd79a1e4b6";
pub(crate) const FINALIZE_COMMAND_DIGEST: &str = "sha256:d23f73d3454649c1d77a98153f9f786ec39da037c658009cd031488dc0ae3fbb";
pub(crate) const FIND_REPLAY_DIGEST: &str = "sha256:8a9052feb2936626e496de91aa52b8c53053dbf340472df6ac0eadb3ef94aaaa";
pub(crate) const LOCK_APPOINTMENT_DIGEST: &str = "sha256:4ff6ee88bf1ad5b39863ff86a8cda0620bbb9c372e6d3a777411c99b3f98668e";
pub(crate) const RECORD_ARRIVAL_DIGEST: &str = "sha256:df6e4445ec18b4917d13d13b2b565fa21704e5e0425255b46f630a585a2273dd";

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
            check_in_id: row.decode("check_in_id")?,
        })
    })
}

pub(crate) async fn finalize_command(
    transaction: &mut Transaction,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    check_in_id: wamn_postgres_statements::Uuid,
    status: String,
    arrived_at: wamn_postgres_statements::TimestampTz,
) -> Result<FinalizeCommandRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(FINALIZE_COMMAND_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(canonical_command),
        wamn_postgres_statements::into_sql_value(check_in_id),
        wamn_postgres_statements::into_sql_value(status),
        wamn_postgres_statements::into_sql_value(arrived_at),
    ]).await?;
    wamn_postgres_statements::decode_one(FINALIZE_COMMAND_DIGEST, rows, |row| {
        Ok(FinalizeCommandRow {
            status: row.decode("status")?,
            arrived_at: row.decode("arrived_at")?,
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
            check_in_id: row.decode("check_in_id")?,
            status: row.decode("status")?,
            arrived_at: row.decode("arrived_at")?,
        })
    })
}

pub(crate) async fn lock_appointment(
    transaction: &mut Transaction,
    appointment_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LockAppointmentRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(LOCK_APPOINTMENT_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(appointment_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(LOCK_APPOINTMENT_DIGEST, rows, |row| {
        Ok(LockAppointmentRow {
            id: row.decode("id")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn record_arrival(
    transaction: &mut Transaction,
    appointment_id: wamn_postgres_statements::Uuid,
    arrived_at: wamn_postgres_statements::TimestampTz,
) -> Result<Option<RecordArrivalRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(RECORD_ARRIVAL_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(appointment_id),
        wamn_postgres_statements::into_sql_value(arrived_at),
    ]).await?;
    wamn_postgres_statements::decode_optional(RECORD_ARRIVAL_DIGEST, rows, |row| {
        Ok(RecordArrivalRow {
            id: row.decode("id")?,
            status: row.decode("status")?,
            arrived_at: row.decode("arrived_at")?,
        })
    })
}
