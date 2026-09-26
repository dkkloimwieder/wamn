// @generated from migration IR; do not edit.

#[derive(Debug)]
pub struct ClaimSampleRow {
    pub sample_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct FindSampleRow {
    pub canonical_command: Vec<u8>,
    pub sample_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct RecordSampleRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const CLAIM_SAMPLE_DIGEST: &str =
    "sha256:88ccbe5f6ccb991ba2b26117272b1c96676962cb4439b3d72a5d42d2819338a8";
pub(crate) const FIND_SAMPLE_DIGEST: &str =
    "sha256:1990abfe831baae51ce7f8f925109a88c1e9730cf529e282acfde068701ade16";
pub(crate) const RECORD_SAMPLE_DIGEST: &str =
    "sha256:052844163944f0d54f245924379b71eb15f1fbf2f0536e9f497e7816695a80dc";

/// One claim and its work, with no commit before finalization.
#[derive(Debug)]
pub(crate) struct PendingClaim {
    transaction: wamn_postgres_statements::Transaction,
}

/// Transfer the open transaction into this command's claim scope.
pub(crate) fn begin_claim(transaction: wamn_postgres_statements::Transaction) -> PendingClaim {
    PendingClaim { transaction }
}

/// A finalized claim whose transaction can now commit.
#[derive(Debug)]
pub(crate) struct FinalizedClaim {
    transaction: wamn_postgres_statements::Transaction,
    pub row: RecordSampleRow,
}

impl FinalizedClaim {
    /// Commit the claim and its work together.
    pub(crate) async fn commit(self) -> Result<(), wamn_postgres_statements::StatementError> {
        self.transaction.commit().await
    }
}

impl PendingClaim {
    /// Select the exact nested operation admitted to use this transaction.
    pub(crate) async fn select_participant(
        &mut self,
        operation: &str,
    ) -> Result<(), wamn_postgres_statements::StatementError> {
        self.transaction.select_participant(operation).await
    }
}

pub(crate) async fn claim_sample(
    claim: &mut PendingClaim,
    canonical_command: Vec<u8>,
    idempotency_key: String,
) -> Result<Option<ClaimSampleRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            CLAIM_SAMPLE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(canonical_command),
                wamn_postgres_statements::into_sql_value(idempotency_key),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_optional(CLAIM_SAMPLE_DIGEST, rows, |row| {
        Ok(ClaimSampleRow {
            sample_id: row.decode("sample_id")?,
        })
    })
}

pub(crate) async fn find_sample(
    claim: &mut PendingClaim,
    idempotency_key: String,
) -> Result<Option<FindSampleRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            FIND_SAMPLE_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(idempotency_key)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(FIND_SAMPLE_DIGEST, rows, |row| {
        Ok(FindSampleRow {
            canonical_command: row.decode("canonical_command")?,
            sample_id: row.decode("sample_id")?,
        })
    })
}

pub(crate) async fn record_sample(
    mut claim: PendingClaim,
    id: wamn_postgres_statements::Uuid,
    frame: String,
    captured_at: wamn_postgres_statements::TimestampTz,
) -> Result<FinalizedClaim, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            RECORD_SAMPLE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(frame),
                wamn_postgres_statements::into_sql_value(captured_at),
            ],
        )
        .await?;
    let row = wamn_postgres_statements::decode_one(RECORD_SAMPLE_DIGEST, rows, |row| {
        Ok(RecordSampleRow {
            id: row.decode("id")?,
        })
    })?;
    Ok(FinalizedClaim {
        transaction: claim.transaction,
        row,
    })
}
