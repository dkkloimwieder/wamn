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
}

#[derive(Debug)]
pub struct LockPackagingRow {
    pub id: wamn_postgres_statements::Uuid,
    pub r#type: String,
    pub code: String,
    pub location_id: wamn_postgres_statements::Uuid,
    pub lifecycle: String,
    pub row_version: i32,
}

#[derive(Debug)]
pub struct OpenInventoryRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const APPLY_DIGEST: &str =
    "sha256:492b5e49724105b0b62a0404a5c6cc5f1d3710684a91d5a6bd7794b673b6dd1a";
pub(crate) const CLAIM_COMMAND_DIGEST: &str =
    "sha256:7b15da5c2cbb4146d19d50ef49d3c03376e85927640eff18e958c1ad6051f6bc";
pub(crate) const FINALIZE_COMMAND_DIGEST: &str =
    "sha256:7f4df0e0959b3a85a32c125c149ba7bbb97b512c2c93df54e27f96794aa8dc75";
pub(crate) const FIND_REPLAY_DIGEST: &str =
    "sha256:811b36e3402f5ab987b830e5a6190b89ae192ac9c7c2215e0a07059acdcd9039";
pub(crate) const LOCK_PACKAGING_DIGEST: &str =
    "sha256:5625ca25ced6e63f3b2013932b6ec96ee4cdf163ff8787d53a7e9c5b3860633b";
pub(crate) const OPEN_INVENTORY_DIGEST: &str =
    "sha256:0ab130e08aeaf8a083752857176af9ac2c656b6dd0ffa11a14a9b6660dfa3223";

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
    packaging_id: wamn_postgres_statements::Uuid,
) -> Result<ApplyRow, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            APPLY_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(packaging_id)],
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
        })
    })
}

pub(crate) async fn lock_packaging(
    claim: &mut PendingClaim,
    packaging_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LockPackagingRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            LOCK_PACKAGING_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(packaging_id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(LOCK_PACKAGING_DIGEST, rows, |row| {
        Ok(LockPackagingRow {
            id: row.decode("id")?,
            r#type: row.decode("type")?,
            code: row.decode("code")?,
            location_id: row.decode("location_id")?,
            lifecycle: row.decode("lifecycle")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn open_inventory(
    claim: &mut PendingClaim,
    packaging_id: wamn_postgres_statements::Uuid,
) -> Result<Option<OpenInventoryRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            OPEN_INVENTORY_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(packaging_id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(OPEN_INVENTORY_DIGEST, rows, |row| {
        Ok(OpenInventoryRow {
            id: row.decode("id")?,
        })
    })
}
