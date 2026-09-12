// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct ClaimCommandRow {
    pub carrier_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct FinalizeCommandRow {
    pub finalized: Option<bool>,
}

#[derive(Debug)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub carrier_id: wamn_postgres_statements::Uuid,
    pub finalized: Option<bool>,
}

#[derive(Debug)]
pub(crate) struct InsertCarrierRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const CLAIM_COMMAND_DIGEST: &str = "sha256:4636d7069200e56170361bc2742ac0d4d0a89dd135accca43d0c41a1060e2bad";
pub(crate) const FINALIZE_COMMAND_DIGEST: &str = "sha256:feca5a21b18c7c380f77e7da2cbe5c2caa601db1d8a235fcc9cd2e91bb654196";
pub(crate) const FIND_REPLAY_DIGEST: &str = "sha256:56c7965b001ab29fbd5b55eac2033c4a2b6379deaef64e3bc25416a090fb09b6";
pub(crate) const INSERT_CARRIER_DIGEST: &str = "sha256:9ad8f02f94d3aeddf45e0c3f1bd444f554055c46f1e385e8a33c06e1facedb1c";

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
            carrier_id: row.decode("carrier_id")?,
        })
    })
}

pub(crate) async fn finalize_command(
    transaction: &mut Transaction,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    carrier_id: wamn_postgres_statements::Uuid,
) -> Result<FinalizeCommandRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(FINALIZE_COMMAND_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(canonical_command),
        wamn_postgres_statements::into_sql_value(carrier_id),
    ]).await?;
    wamn_postgres_statements::decode_one(FINALIZE_COMMAND_DIGEST, rows, |row| {
        Ok(FinalizeCommandRow {
            finalized: row.decode("finalized")?,
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
            carrier_id: row.decode("carrier_id")?,
            finalized: row.decode("finalized")?,
        })
    })
}

pub(crate) async fn insert_carrier(
    transaction: &mut Transaction,
    carrier_id: wamn_postgres_statements::Uuid,
    name: String,
) -> Result<InsertCarrierRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(INSERT_CARRIER_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(carrier_id),
        wamn_postgres_statements::into_sql_value(name),
    ]).await?;
    wamn_postgres_statements::decode_one(INSERT_CARRIER_DIGEST, rows, |row| {
        Ok(InsertCarrierRow {
            id: row.decode("id")?,
        })
    })
}
