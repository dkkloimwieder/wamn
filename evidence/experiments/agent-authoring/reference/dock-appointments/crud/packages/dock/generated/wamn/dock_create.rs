// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct ClaimCommandRow {
    pub dock_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct FinalizeCommandRow {
    pub finalized: Option<bool>,
}

#[derive(Debug)]
pub(crate) struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub dock_id: wamn_postgres_statements::Uuid,
    pub finalized: Option<bool>,
}

#[derive(Debug)]
pub(crate) struct InsertDockRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const CLAIM_COMMAND_DIGEST: &str = "sha256:df1f37583c83863574cb8100d063490d3fa8cfb4652b748a6066b941b0d04b90";
pub(crate) const FINALIZE_COMMAND_DIGEST: &str = "sha256:9552092da227c049936f9d5bccfdc1ebe15c6c76079a26df53a11d2ebc70b121";
pub(crate) const FIND_REPLAY_DIGEST: &str = "sha256:be93e32089ecd5b35acd3f011c543070a24ddc30ef228e1c7595180e37e9b260";
pub(crate) const INSERT_DOCK_DIGEST: &str = "sha256:faca6c8b1b7f7742ce154621335c922bbb722778be96848bb184c334acdbcda6";

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
            dock_id: row.decode("dock_id")?,
        })
    })
}

pub(crate) async fn finalize_command(
    transaction: &mut Transaction,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    dock_id: wamn_postgres_statements::Uuid,
) -> Result<FinalizeCommandRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(FINALIZE_COMMAND_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(idempotency_key),
        wamn_postgres_statements::into_sql_value(canonical_command),
        wamn_postgres_statements::into_sql_value(dock_id),
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
            dock_id: row.decode("dock_id")?,
            finalized: row.decode("finalized")?,
        })
    })
}

pub(crate) async fn insert_dock(
    transaction: &mut Transaction,
    dock_id: wamn_postgres_statements::Uuid,
    name: String,
) -> Result<InsertDockRow, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(INSERT_DOCK_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(dock_id),
        wamn_postgres_statements::into_sql_value(name),
    ]).await?;
    wamn_postgres_statements::decode_one(INSERT_DOCK_DIGEST, rows, |row| {
        Ok(InsertDockRow {
            id: row.decode("id")?,
        })
    })
}
