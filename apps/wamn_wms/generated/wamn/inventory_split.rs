// @generated from migration IR; do not edit.

#[derive(Debug)]
pub struct ApplyRow {
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
    pub new_inventory_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct CreateInventoryRow {
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
pub struct FinalizeCommandRow {
    pub result: Option<String>,
}

#[derive(Debug)]
pub struct FindReplayRow {
    pub canonical_command: Vec<u8>,
    pub result: Option<String>,
    pub operation_id: wamn_postgres_statements::Uuid,
    pub new_inventory_id: wamn_postgres_statements::Uuid,
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

pub(crate) const APPLY_DIGEST: &str =
    "sha256:d7baa70848354f550a74cc3578958fc3d15be18cc857ee4d2dda04d524fa735c";
pub(crate) const CLAIM_COMMAND_DIGEST: &str =
    "sha256:284545cbbfd286d9d24de92cc6fbd9a824a489800b392ee70f67f4d020583255";
pub(crate) const CREATE_INVENTORY_DIGEST: &str =
    "sha256:12d5a651688c89251807486817b9515b7e6cdd69e940a17654e46020a3d670fb";
pub(crate) const FINALIZE_COMMAND_DIGEST: &str =
    "sha256:5062b0b6425ef75c1aa24b4849725f42f7c9658ac0fb75b047e245e82bc3bbc8";
pub(crate) const FIND_REPLAY_DIGEST: &str =
    "sha256:577bd6e23d8ebf7e5eb3587215a8ddce70013010beedb910ae91c193a21b1874";
pub(crate) const INSERT_TRANSACTION_DIGEST: &str =
    "sha256:16e3f0768ea80f54e7cfa9bd0a1a1c1940cef9a2143b1db7d3f23845fbe4bccd";
pub(crate) const LOCK_INVENTORY_DIGEST: &str =
    "sha256:fa2fa96abd2b992f7fa50dd2cb2ae5194930f891534f7220c122bb3ce5645351";
pub(crate) const LOCK_PACKAGING_DIGEST: &str =
    "sha256:84b8b00d46ee3266ea0fdeee6553b218aa79cf40ae3d8d8fe4a8d40093da52ca";

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
    inventory_id: wamn_postgres_statements::Uuid,
    quantity: wamn_postgres_statements::Numeric,
) -> Result<Option<ApplyRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            APPLY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(inventory_id),
                wamn_postgres_statements::into_sql_value(quantity),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_optional(APPLY_DIGEST, rows, |row| {
        Ok(ApplyRow {
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
            new_inventory_id: row.decode("new_inventory_id")?,
        })
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the parameters are the statement's bind list"
)]
pub(crate) async fn create_inventory(
    claim: &mut PendingClaim,
    id: wamn_postgres_statements::Uuid,
    product_id: wamn_postgres_statements::Uuid,
    packaging_id: wamn_postgres_statements::Uuid,
    location_id: wamn_postgres_statements::Uuid,
    quantity: wamn_postgres_statements::Numeric,
    disposition: String,
    lifecycle: String,
) -> Result<CreateInventoryRow, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            CREATE_INVENTORY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(product_id),
                wamn_postgres_statements::into_sql_value(packaging_id),
                wamn_postgres_statements::into_sql_value(location_id),
                wamn_postgres_statements::into_sql_value(quantity),
                wamn_postgres_statements::into_sql_value(disposition),
                wamn_postgres_statements::into_sql_value(lifecycle),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(CREATE_INVENTORY_DIGEST, rows, |row| {
        Ok(CreateInventoryRow {
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
            new_inventory_id: row.decode("new_inventory_id")?,
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
    to_product_id: wamn_postgres_statements::Uuid,
    to_packaging_id: wamn_postgres_statements::Uuid,
    to_location_id: wamn_postgres_statements::Uuid,
    to_quantity: wamn_postgres_statements::Numeric,
    to_disposition: String,
    to_lifecycle: String,
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
                wamn_postgres_statements::into_sql_value(to_product_id),
                wamn_postgres_statements::into_sql_value(to_packaging_id),
                wamn_postgres_statements::into_sql_value(to_location_id),
                wamn_postgres_statements::into_sql_value(to_quantity),
                wamn_postgres_statements::into_sql_value(to_disposition),
                wamn_postgres_statements::into_sql_value(to_lifecycle),
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
    inventory_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LockInventoryRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            LOCK_INVENTORY_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(inventory_id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(LOCK_INVENTORY_DIGEST, rows, |row| {
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
    from_packaging_id: wamn_postgres_statements::Uuid,
    to_packaging_id: wamn_postgres_statements::Uuid,
) -> Result<Vec<LockPackagingRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            LOCK_PACKAGING_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(from_packaging_id),
                wamn_postgres_statements::into_sql_value(to_packaging_id),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(LOCK_PACKAGING_DIGEST, rows, |row| {
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
