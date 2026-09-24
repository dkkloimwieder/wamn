// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct ProductRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub product_code: String,
    pub row_version: i32,
}

#[derive(Debug)]
pub struct ProductCreateClaimRow {
    pub product_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct ProductCreateReplayRow {
    pub canonical_command: Vec<u8>,
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub product_code: String,
    pub row_version: i32,
}

#[derive(Debug)]
pub struct ProductUpdateRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i32>,
    pub created_at: Option<wamn_postgres_statements::TimestampTz>,
    pub id: Option<wamn_postgres_statements::Uuid>,
    pub product_code: Option<String>,
    pub row_version: Option<i32>,
}

pub(crate) const CREATE_0_DIGEST: &str =
    "sha256:c7c00f29183723beaf711496f5979e6f2b51913ed1178c371d9b2f2712cc81b4";
pub(crate) const CREATE_1_DIGEST: &str =
    "sha256:7c559ed45ec4cd2712dc5e5c12712bde51323edb16282553dd70744d0a83994e";
pub(crate) const CREATE_2_DIGEST: &str =
    "sha256:0504b7c126d4bdfae4b768e46b083e9f02c29d4fe461447962d2dc5fcfb50272";
pub(crate) const GET_DIGEST: &str =
    "sha256:93087bc679532d835ef2520dcba91bd0a4df2788792926c814269641944b9157";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:041738b4ceef42dfb0b4d1c3c9a0eef2dc130c57ecd54040019fa7a15d51d884";
pub(crate) const UPDATE_DIGEST: &str =
    "sha256:657b27225661e113aad5fca5ad3401b399a37bdb3707b3d78b172465369ffcad";

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
    pub row: ProductRow,
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

pub(crate) const CREATE_UNIQUE_CONSTRAINTS: &[&str] =
    &["product_id_pkey", "product_product_code_key"];
pub(crate) const CREATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const CREATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const CREATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_UNIQUE_CONSTRAINTS: &[&str] = &["product_product_code_key"];
pub(crate) const UPDATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<ProductRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(ProductRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            product_code: row.decode("product_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    product_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<ProductRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            QUERY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(product_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_DIGEST, rows, |row| {
        Ok(ProductRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            product_code: row.decode("product_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn create_claim(
    claim: &mut PendingClaim,
    idempotency_key: String,
    canonical_command: Vec<u8>,
) -> Result<Option<ProductCreateClaimRow>, wamn_postgres_statements::StatementError> {
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
        Ok(ProductCreateClaimRow {
            product_id: row.decode("product_id")?,
        })
    })
}

pub(crate) async fn create_replay(
    claim: &mut PendingClaim,
    idempotency_key: String,
) -> Result<Option<ProductCreateReplayRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            CREATE_1_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(idempotency_key)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(CREATE_1_DIGEST, rows, |row| {
        Ok(ProductCreateReplayRow {
            canonical_command: row.decode("canonical_command")?,
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            product_code: row.decode("product_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn create(
    mut claim: PendingClaim,
    id: wamn_postgres_statements::Uuid,
    product_code: String,
) -> Result<FinalizedClaim, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            CREATE_2_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(product_code),
            ],
        )
        .await?;
    let row = wamn_postgres_statements::decode_one(CREATE_2_DIGEST, rows, |row| {
        Ok(ProductRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            product_code: row.decode("product_code")?,
            row_version: row.decode("row_version")?,
        })
    })?;
    Ok(FinalizedClaim {
        transaction: claim.transaction,
        row,
    })
}

pub(crate) async fn update(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
    expected_row_version: i32,
    product_code_present: bool,
    product_code_value: Option<String>,
) -> Result<ProductUpdateRow, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            UPDATE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(expected_row_version),
                wamn_postgres_statements::into_sql_value(product_code_present),
                wamn_postgres_statements::into_sql_value(product_code_value),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(UPDATE_DIGEST, rows, |row| {
        Ok(ProductUpdateRow {
            outcome: row.decode("outcome")?,
            observed_row_version: row.decode("observed_row_version")?,
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            product_code: row.decode("product_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}
