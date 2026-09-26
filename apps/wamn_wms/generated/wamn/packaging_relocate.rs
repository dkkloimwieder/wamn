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
pub struct ApplyInventoryRow {
    pub id: wamn_postgres_statements::Uuid,
    pub product_id: wamn_postgres_statements::Uuid,
    pub packaging_id: wamn_postgres_statements::Uuid,
    pub location_id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
    pub disposition: String,
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
pub struct InsertTransactionRow {
    pub id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct LockInventoryRow {
    pub id: wamn_postgres_statements::Uuid,
    pub product_id: wamn_postgres_statements::Uuid,
    pub packaging_id: wamn_postgres_statements::Uuid,
    pub location_id: wamn_postgres_statements::Uuid,
    pub quantity: wamn_postgres_statements::Numeric,
    pub disposition: String,
    pub lifecycle: String,
    pub row_version: i32,
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

#[derive(Debug)]
pub struct ValidateLocationRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const APPLY_DIGEST: &str =
    "sha256:0fdfc491e96bf65892f231a4d4575a082e70e5f7f4d4b0727a7be990f97a6287";
pub(crate) const APPLY_INVENTORY_DIGEST: &str =
    "sha256:c7b5e982d4f94db2611473a350e097f3f75ad1a6af1ef8f1fa1b0879b33baffd";
pub(crate) const CLAIM_COMMAND_DIGEST: &str =
    "sha256:a1aab95102475b2240de2c3e034637744e051c8af006a5ec057c13cc1c965bef";
pub(crate) const FINALIZE_COMMAND_DIGEST: &str =
    "sha256:f437d9aa58b6f0eb258e32a004796bad66e36fdbf4b8caf023f78eee9b98296b";
pub(crate) const FIND_REPLAY_DIGEST: &str =
    "sha256:882c515f6b238a0f5c038aebacff0ca8d012cf05466b55994648f02454a1f4d4";
pub(crate) const INSERT_TRANSACTION_DIGEST: &str =
    "sha256:6ee935c7d7e32977bf76941da5fab13d3d22e03dadec21df25d0a79b62b76f79";
pub(crate) const LOCK_INVENTORY_DIGEST: &str =
    "sha256:00348f64aff290924f3948b98bae386944b82d4b915b04fe356732f075d7b248";
pub(crate) const LOCK_PACKAGING_DIGEST: &str =
    "sha256:5625ca25ced6e63f3b2013932b6ec96ee4cdf163ff8787d53a7e9c5b3860633b";
pub(crate) const OPEN_INVENTORY_DIGEST: &str =
    "sha256:307c5f62bc877e61be6f9f14346d93563d471998f42868d1878346adde55c48d";
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
    packaging_id: wamn_postgres_statements::Uuid,
    to_location_id: wamn_postgres_statements::Uuid,
) -> Result<ApplyRow, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            APPLY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(packaging_id),
                wamn_postgres_statements::into_sql_value(to_location_id),
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

pub(crate) async fn apply_inventory(
    claim: &mut PendingClaim,
    inventory_id: wamn_postgres_statements::Uuid,
    to_location_id: wamn_postgres_statements::Uuid,
) -> Result<ApplyInventoryRow, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            APPLY_INVENTORY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(inventory_id),
                wamn_postgres_statements::into_sql_value(to_location_id),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(APPLY_INVENTORY_DIGEST, rows, |row| {
        Ok(ApplyInventoryRow {
            id: row.decode("id")?,
            product_id: row.decode("product_id")?,
            packaging_id: row.decode("packaging_id")?,
            location_id: row.decode("location_id")?,
            quantity: row.decode("quantity")?,
            disposition: row.decode("disposition")?,
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

#[expect(
    clippy::too_many_arguments,
    reason = "the parameters are the statement's bind list"
)]
pub(crate) async fn insert_transaction(
    claim: &mut PendingClaim,
    operation_id: wamn_postgres_statements::Uuid,
    inventory_id: wamn_postgres_statements::Uuid,
    from_inventory_id: wamn_postgres_statements::Uuid,
    to_inventory_id: wamn_postgres_statements::Uuid,
    from_product_id: Option<wamn_postgres_statements::Uuid>,
    from_packaging_id: Option<wamn_postgres_statements::Uuid>,
    from_location_id: Option<wamn_postgres_statements::Uuid>,
    from_quantity: wamn_postgres_statements::Numeric,
    from_disposition: Option<String>,
    from_lifecycle: Option<String>,
    occurred_at: wamn_postgres_statements::TimestampTz,
    reason: Option<String>,
) -> Result<InsertTransactionRow, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            INSERT_TRANSACTION_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(operation_id),
                wamn_postgres_statements::into_sql_value(inventory_id),
                wamn_postgres_statements::into_sql_value(from_inventory_id),
                wamn_postgres_statements::into_sql_value(to_inventory_id),
                wamn_postgres_statements::into_sql_value(from_product_id),
                wamn_postgres_statements::into_sql_value(from_packaging_id),
                wamn_postgres_statements::into_sql_value(from_location_id),
                wamn_postgres_statements::into_sql_value(from_quantity),
                wamn_postgres_statements::into_sql_value(from_disposition),
                wamn_postgres_statements::into_sql_value(from_lifecycle),
                wamn_postgres_statements::into_sql_value(occurred_at),
                wamn_postgres_statements::into_sql_value(reason),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(INSERT_TRANSACTION_DIGEST, rows, |row| {
        Ok(InsertTransactionRow {
            id: row.decode("id")?,
        })
    })
}

pub(crate) async fn lock_inventory(
    claim: &mut PendingClaim,
    packaging_id: wamn_postgres_statements::Uuid,
) -> Result<Vec<LockInventoryRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            LOCK_INVENTORY_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(packaging_id)],
        )
        .await?;
    wamn_postgres_statements::decode_all(LOCK_INVENTORY_DIGEST, rows, |row| {
        Ok(LockInventoryRow {
            id: row.decode("id")?,
            product_id: row.decode("product_id")?,
            packaging_id: row.decode("packaging_id")?,
            location_id: row.decode("location_id")?,
            quantity: row.decode("quantity")?,
            disposition: row.decode("disposition")?,
            lifecycle: row.decode("lifecycle")?,
            row_version: row.decode("row_version")?,
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
) -> Result<Vec<OpenInventoryRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            OPEN_INVENTORY_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(packaging_id)],
        )
        .await?;
    wamn_postgres_statements::decode_all(OPEN_INVENTORY_DIGEST, rows, |row| {
        Ok(OpenInventoryRow {
            id: row.decode("id")?,
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
