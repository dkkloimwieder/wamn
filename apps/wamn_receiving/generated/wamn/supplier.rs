// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct SupplierRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub name: String,
}

#[derive(Debug)]
pub struct SupplierCreateClaimRow {
    pub supplier_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct SupplierCreateReplayRow {
    pub canonical_command: Vec<u8>,
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub name: String,
}

pub(crate) const CREATE_0_DIGEST: &str =
    "sha256:f8f816a2a83e16074ca9f0ebe3e305704b9c1119b4e9e2c94cbfdd05a18fd607";
pub(crate) const CREATE_1_DIGEST: &str =
    "sha256:5af13a4e914141ca60a8801e060c09dfd274827ce3923f2da892f2a04969e2cc";
pub(crate) const CREATE_2_DIGEST: &str =
    "sha256:910ed1d9ba015f04762eeb3bc1f97afa365e4a9e3c9ed75f238610c9654f7ddb";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:c3b2e06e9e6e007917f55f4a3e48aaddb8008014687da62f64d2df159bba954e";

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
    pub row: SupplierRow,
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

pub(crate) const CREATE_UNIQUE_CONSTRAINTS: &[&str] = &["supplier_id_pkey"];
pub(crate) const CREATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const CREATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const CREATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<
    wamn_postgres_statements::RowStream<SupplierRow>,
    wamn_postgres_statements::StatementError,
> {
    connection
        .run_stream(
            QUERY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(SupplierRow {
                    created_at: row.decode("created_at")?,
                    id: row.decode("id")?,
                    name: row.decode("name")?,
                })
            },
        )
        .await
}

pub(crate) async fn create_claim(
    claim: &mut PendingClaim,
    idempotency_key: String,
    canonical_command: Vec<u8>,
) -> Result<Option<SupplierCreateClaimRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            CREATE_0_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(idempotency_key),
                wamn_postgres_statements::into_sql_value(canonical_command),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_optional(CREATE_0_DIGEST, rows, |row| {
        Ok(SupplierCreateClaimRow {
            supplier_id: row.decode("supplier_id")?,
        })
    })
}

pub(crate) async fn create_replay(
    claim: &mut PendingClaim,
    idempotency_key: String,
) -> Result<Option<SupplierCreateReplayRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            CREATE_1_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(idempotency_key)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(CREATE_1_DIGEST, rows, |row| {
        Ok(SupplierCreateReplayRow {
            canonical_command: row.decode("canonical_command")?,
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            name: row.decode("name")?,
        })
    })
}

pub(crate) async fn create(
    mut claim: PendingClaim,
    id: wamn_postgres_statements::Uuid,
    name: String,
) -> Result<FinalizedClaim, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            CREATE_2_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(name),
            ],
        )
        .await?;
    let row = wamn_postgres_statements::decode_one(CREATE_2_DIGEST, rows, |row| {
        Ok(SupplierRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            name: row.decode("name")?,
        })
    })?;
    Ok(FinalizedClaim {
        transaction: claim.transaction,
        row,
    })
}
