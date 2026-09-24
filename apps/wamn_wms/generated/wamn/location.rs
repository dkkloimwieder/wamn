// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct LocationRow {
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub location_code: String,
    pub row_version: i32,
}

#[derive(Debug)]
pub struct LocationCreateClaimRow {
    pub location_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct LocationCreateReplayRow {
    pub canonical_command: Vec<u8>,
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub id: wamn_postgres_statements::Uuid,
    pub location_code: String,
    pub row_version: i32,
}

#[derive(Debug)]
pub struct LocationUpdateRow {
    pub outcome: Option<String>,
    pub observed_row_version: Option<i32>,
    pub created_at: Option<wamn_postgres_statements::TimestampTz>,
    pub id: Option<wamn_postgres_statements::Uuid>,
    pub location_code: Option<String>,
    pub row_version: Option<i32>,
}

pub(crate) const CREATE_0_DIGEST: &str =
    "sha256:885456d28169f21de2218028b6b1fdab9e97ac4cd45e85ce1e9e0a11896bf454";
pub(crate) const CREATE_1_DIGEST: &str =
    "sha256:f6cc887f8968ec0fe5ca13157e2545c9fddabbb49396a6ad0379ec5ee2f46d58";
pub(crate) const CREATE_2_DIGEST: &str =
    "sha256:1e091fb9433f06e605acb619a37b723a65bd6c7c3d0083c5033147fda9c6de90";
pub(crate) const GET_DIGEST: &str =
    "sha256:61ac14096e05333a1144902bc75008c04282a56c79f864c6b70c0bdf6d4cfb43";
pub(crate) const QUERY_DIGEST: &str =
    "sha256:a6de5ac55dd7ca3338920967d4d96c32c5ce5587678acd25a5118de40cda6bc8";
pub(crate) const UPDATE_DIGEST: &str =
    "sha256:44379e3dce960c2cb46c2e7a359305f33bc830a1ae3b91c56a9382f06bcc4b07";

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
    pub row: LocationRow,
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
    &["location_id_pkey", "location_location_code_key"];
pub(crate) const CREATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const CREATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const CREATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_UNIQUE_CONSTRAINTS: &[&str] = &["location_location_code_key"];
pub(crate) const UPDATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<LocationRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(LocationRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_code: row.decode("location_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    location_code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<Vec<LocationRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            QUERY_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(location_code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_all(QUERY_DIGEST, rows, |row| {
        Ok(LocationRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_code: row.decode("location_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn create_claim(
    claim: &mut PendingClaim,
    idempotency_key: String,
    canonical_command: Vec<u8>,
) -> Result<Option<LocationCreateClaimRow>, wamn_postgres_statements::StatementError> {
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
        Ok(LocationCreateClaimRow {
            location_id: row.decode("location_id")?,
        })
    })
}

pub(crate) async fn create_replay(
    claim: &mut PendingClaim,
    idempotency_key: String,
) -> Result<Option<LocationCreateReplayRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            CREATE_1_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(idempotency_key)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(CREATE_1_DIGEST, rows, |row| {
        Ok(LocationCreateReplayRow {
            canonical_command: row.decode("canonical_command")?,
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_code: row.decode("location_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}

pub(crate) async fn create(
    mut claim: PendingClaim,
    id: wamn_postgres_statements::Uuid,
    location_code: String,
) -> Result<FinalizedClaim, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            CREATE_2_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(location_code),
            ],
        )
        .await?;
    let row = wamn_postgres_statements::decode_one(CREATE_2_DIGEST, rows, |row| {
        Ok(LocationRow {
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_code: row.decode("location_code")?,
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
    location_code_present: bool,
    location_code_value: Option<String>,
) -> Result<LocationUpdateRow, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            UPDATE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(expected_row_version),
                wamn_postgres_statements::into_sql_value(location_code_present),
                wamn_postgres_statements::into_sql_value(location_code_value),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(UPDATE_DIGEST, rows, |row| {
        Ok(LocationUpdateRow {
            outcome: row.decode("outcome")?,
            observed_row_version: row.decode("observed_row_version")?,
            created_at: row.decode("created_at")?,
            id: row.decode("id")?,
            location_code: row.decode("location_code")?,
            row_version: row.decode("row_version")?,
        })
    })
}
