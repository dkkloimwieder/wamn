// @generated from migration IR; do not edit.

#[derive(Debug)]
pub struct ApplyRow {
    pub id: wamn_postgres_statements::Uuid,
    pub r#type: String,
    pub code: String,
    pub location_id: wamn_postgres_statements::Uuid,
    pub lifecycle: String,
    pub row_version: i32,
}

#[derive(Debug)]
pub struct ClaimCommandRow {
    pub operation_id: wamn_postgres_statements::Uuid,
    pub packaging_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct FinalizeCommandRow {
    pub result: Option<String>,
}

#[derive(Debug)]
pub struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub result: Option<String>,
    pub operation_id: wamn_postgres_statements::Uuid,
    pub packaging_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct ValidateLocationRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const APPLY_DIGEST: &str =
    "sha256:ed9c9103dc7a247ded1bbc918a72d0e6d4bd3567465f60e9104bccd4b05c3adc";
pub(crate) const CLAIM_COMMAND_DIGEST: &str =
    "sha256:46c3b517c87087cc115ce30ba03048bbbe971171e6de143ed90fbe8c5e496ee9";
pub(crate) const FINALIZE_COMMAND_DIGEST: &str =
    "sha256:206af26e2a31cfc622cb475db583b3907cf193ef0c44393000b99b312ef2c54e";
pub(crate) const FIND_REPLAY_DIGEST: &str =
    "sha256:51d59b1aa11b7ede557d8b8a01250117340827bd586444e50347f2c8eadbb28c";
pub(crate) const VALIDATE_LOCATION_DIGEST: &str =
    "sha256:b2eea184b3fb6ad278127865c246988c5ff45c884ed031535688d89d14009de5";

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
    pub row: FinalizeCommandRow,
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

pub(crate) async fn apply(
    claim: &mut PendingClaim,
    id: wamn_postgres_statements::Uuid,
    r#type: String,
    code: String,
    location_id: wamn_postgres_statements::Uuid,
) -> Result<ApplyRow, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            APPLY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(r#type),
                wamn_postgres_statements::into_sql_value(code),
                wamn_postgres_statements::into_sql_value(location_id),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(APPLY_DIGEST, rows, |row| {
        Ok(ApplyRow {
            id: row.decode("id")?,
            r#type: row.decode("type")?,
            code: row.decode("code")?,
            location_id: row.decode("location_id")?,
            lifecycle: row.decode("lifecycle")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn claim_command(
    claim: &mut PendingClaim,
    idempotency_key: String,
    canonical_command: Vec<u8>,
) -> Result<Option<ClaimCommandRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            CLAIM_COMMAND_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(idempotency_key),
                wamn_postgres_statements::into_sql_value(canonical_command),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_optional(CLAIM_COMMAND_DIGEST, rows, |row| {
        Ok(ClaimCommandRow {
            operation_id: row.decode("operation_id")?,
            packaging_id: row.decode("packaging_id")?,
        })
    })
}

pub(crate) async fn finalize_command(
    mut claim: PendingClaim,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    operation_id: wamn_postgres_statements::Uuid,
    result: String,
) -> Result<FinalizedClaim, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            FINALIZE_COMMAND_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(idempotency_key),
                wamn_postgres_statements::into_sql_value(canonical_command),
                wamn_postgres_statements::into_sql_value(operation_id),
                wamn_postgres_statements::into_sql_value(result),
            ],
        )
        .await?;
    let row = wamn_postgres_statements::decode_one(FINALIZE_COMMAND_DIGEST, rows, |row| {
        Ok(FinalizeCommandRow {
            result: row.decode("result")?,
        })
    })?;
    Ok(FinalizedClaim {
        transaction: claim.transaction,
        row,
    })
}

pub(crate) async fn find_replay(
    claim: &mut PendingClaim,
    idempotency_key: String,
) -> Result<Option<FindReplayRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            FIND_REPLAY_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(idempotency_key)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(FIND_REPLAY_DIGEST, rows, |row| {
        Ok(FindReplayRow {
            canonical_command: row.decode("canonical_command")?,
            result: row.decode("result")?,
            operation_id: row.decode("operation_id")?,
            packaging_id: row.decode("packaging_id")?,
        })
    })
}

pub(crate) async fn validate_location(
    claim: &mut PendingClaim,
    location_id: wamn_postgres_statements::Uuid,
) -> Result<Option<ValidateLocationRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            VALIDATE_LOCATION_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(location_id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(VALIDATE_LOCATION_DIGEST, rows, |row| {
        Ok(ValidateLocationRow {
            id: row.decode("id")?,
        })
    })
}
