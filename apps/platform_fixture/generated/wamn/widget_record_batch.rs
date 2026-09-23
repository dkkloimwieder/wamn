// @generated from migration IR; do not edit.

#[derive(Debug)]
pub struct ClaimBatchRow {
    pub widget_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct FinalizeBatchRow {
    pub widget_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct FindBatchRow {
    pub canonical_command: Vec<u8>,
    pub widget_id: wamn_postgres_statements::Uuid,
}

pub(crate) const CLAIM_BATCH_DIGEST: &str =
    "sha256:3e98171c4e6c6dc523004f9b5d1578faf08ef8e803282b982a676cdbf1efa77d";
pub(crate) const FINALIZE_BATCH_DIGEST: &str =
    "sha256:d0589a443714be0c322194f12060c02d70f06f9740a210176b75e017b2f4f603";
pub(crate) const FIND_BATCH_DIGEST: &str =
    "sha256:d64a246fe06ccd0f9b4e0528435d6743ed66c1921f5297e332a48b7aa37abeb1";

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
    pub row: FinalizeBatchRow,
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

pub(crate) async fn claim_batch(
    claim: &mut PendingClaim,
    canonical_command: Vec<u8>,
    idempotency_key: String,
) -> Result<Option<ClaimBatchRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            CLAIM_BATCH_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(canonical_command),
                wamn_postgres_statements::into_sql_value(idempotency_key),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_optional(CLAIM_BATCH_DIGEST, rows, |row| {
        Ok(ClaimBatchRow {
            widget_id: row.decode("widget_id")?,
        })
    })
}

pub(crate) async fn finalize_batch(
    mut claim: PendingClaim,
    idempotency_key: String,
) -> Result<FinalizedClaim, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            FINALIZE_BATCH_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(idempotency_key)],
        )
        .await?;
    let row = wamn_postgres_statements::decode_one(FINALIZE_BATCH_DIGEST, rows, |row| {
        Ok(FinalizeBatchRow {
            widget_id: row.decode("widget_id")?,
        })
    })?;
    Ok(FinalizedClaim {
        transaction: claim.transaction,
        row,
    })
}

pub(crate) async fn find_batch(
    claim: &mut PendingClaim,
    idempotency_key: String,
) -> Result<Option<FindBatchRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            FIND_BATCH_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(idempotency_key)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(FIND_BATCH_DIGEST, rows, |row| {
        Ok(FindBatchRow {
            canonical_command: row.decode("canonical_command")?,
            widget_id: row.decode("widget_id")?,
        })
    })
}
