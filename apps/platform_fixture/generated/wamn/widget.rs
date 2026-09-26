// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct WidgetRow {
    pub code: String,
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub edit_version: i64,
    pub id: wamn_postgres_statements::Uuid,
    pub maker_id: Option<wamn_postgres_statements::Uuid>,
    pub note: Option<String>,
}

#[derive(Debug)]
pub struct WidgetCreateClaimRow {
    pub widget_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub struct WidgetCreateReplayRow {
    pub canonical_command: Vec<u8>,
    pub code: String,
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub edit_version: i64,
    pub id: wamn_postgres_statements::Uuid,
    pub maker_id: Option<wamn_postgres_statements::Uuid>,
    pub note: Option<String>,
}

#[derive(Debug)]
pub struct WidgetUpdateRow {
    pub outcome: Option<String>,
    pub observed_edit_version: Option<i64>,
    pub code: Option<String>,
    pub created_at: Option<wamn_postgres_statements::TimestampTz>,
    pub edit_version: Option<i64>,
    pub id: Option<wamn_postgres_statements::Uuid>,
    pub maker_id: Option<wamn_postgres_statements::Uuid>,
    pub note: Option<String>,
}

#[derive(Debug)]
pub struct WidgetDeleteRow {
    pub outcome: Option<String>,
}

pub(crate) const CREATE_0_DIGEST: &str =
    "sha256:6691f98729cc7e27ad7eed11aa67c5f93036228409053700095c6d69cd648bb9";
pub(crate) const CREATE_1_DIGEST: &str =
    "sha256:c721fc72f7dfa53567c952102ca0e2c6ced818b49390194c58d8c568710fd05e";
pub(crate) const CREATE_2_DIGEST: &str =
    "sha256:d9252c8fdfcb9bf3b6c4237b5494d6e85e25d4a5c75785708d966d9c6df8df56";
pub(crate) const DELETE_DIGEST: &str =
    "sha256:4abef9e5b92ef8351c009537cf0446f9710c1d5383b186dbe93a045b623f2fce";
pub(crate) const GET_DIGEST: &str =
    "sha256:9033b3a1caa6ee73ba3aa4a5df84824c1aaf806e3e7c90329e2a9a3324e9b9bf";
pub(crate) const QUERY_0_DIGEST: &str =
    "sha256:74a348533ed0fd6ebcaada6a221f6d7528dd0f347081963a8296c9c260fb9bdd";
pub(crate) const QUERY_1_DIGEST: &str =
    "sha256:2a3850fcad949f88f0e2af627786c9da79663bbf0d524145894634bd3909b76e";
pub(crate) const UPDATE_DIGEST: &str =
    "sha256:a012c26134f98282143795c4c02c39ff6b7db89ef85661c279a275a3125ef540";

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
    pub row: WidgetRow,
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

pub(crate) const CREATE_UNIQUE_CONSTRAINTS: &[&str] = &["widget_code_key", "widget_id_pkey"];
pub(crate) const CREATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &["widget_maker_id_fkey"];
pub(crate) const CREATE_CHECK_CONSTRAINTS: &[&str] = &["widget_code_check"];
pub(crate) const CREATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_UNIQUE_CONSTRAINTS: &[&str] = &["widget_code_key"];
pub(crate) const UPDATE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &["widget_maker_id_fkey"];
pub(crate) const UPDATE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const UPDATE_EXCLUSION_CONSTRAINTS: &[&str] = &[];
pub(crate) const DELETE_UNIQUE_CONSTRAINTS: &[&str] = &[];
pub(crate) const DELETE_FOREIGN_KEY_CONSTRAINTS: &[&str] = &[];
pub(crate) const DELETE_CHECK_CONSTRAINTS: &[&str] = &[];
pub(crate) const DELETE_EXCLUSION_CONSTRAINTS: &[&str] = &[];

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<WidgetRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(WidgetRow {
            code: row.decode("code")?,
            created_at: row.decode("created_at")?,
            edit_version: row.decode("edit_version")?,
            id: row.decode("id")?,
            maker_id: row.decode("maker_id")?,
            note: row.decode("note")?,
        })
    })
}

pub(crate) async fn query_created_at_ascending(
    connection: &mut Connection,
    code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<wamn_postgres_statements::RowStream<WidgetRow>, wamn_postgres_statements::StatementError>
{
    connection
        .run_stream(
            QUERY_0_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(WidgetRow {
                    code: row.decode("code")?,
                    created_at: row.decode("created_at")?,
                    edit_version: row.decode("edit_version")?,
                    id: row.decode("id")?,
                    maker_id: row.decode("maker_id")?,
                    note: row.decode("note")?,
                })
            },
        )
        .await
}

pub(crate) async fn query_created_at_descending(
    connection: &mut Connection,
    code_filter: Option<wamn_postgres_statements::Json>,
    cursor_key: Option<wamn_postgres_statements::TimestampTz>,
    cursor_id: Option<wamn_postgres_statements::Uuid>,
    limit: i64,
) -> Result<wamn_postgres_statements::RowStream<WidgetRow>, wamn_postgres_statements::StatementError>
{
    connection
        .run_stream(
            QUERY_1_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(code_filter),
                wamn_postgres_statements::into_sql_value(cursor_key),
                wamn_postgres_statements::into_sql_value(cursor_id),
                wamn_postgres_statements::into_sql_value(limit),
            ],
            |row| {
                Ok(WidgetRow {
                    code: row.decode("code")?,
                    created_at: row.decode("created_at")?,
                    edit_version: row.decode("edit_version")?,
                    id: row.decode("id")?,
                    maker_id: row.decode("maker_id")?,
                    note: row.decode("note")?,
                })
            },
        )
        .await
}

pub(crate) async fn create_claim(
    claim: &mut PendingClaim,
    idempotency_key: String,
    canonical_command: Vec<u8>,
) -> Result<Option<WidgetCreateClaimRow>, wamn_postgres_statements::StatementError> {
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
        Ok(WidgetCreateClaimRow {
            widget_id: row.decode("widget_id")?,
        })
    })
}

pub(crate) async fn create_replay(
    claim: &mut PendingClaim,
    idempotency_key: String,
) -> Result<Option<WidgetCreateReplayRow>, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            CREATE_1_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(idempotency_key)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(CREATE_1_DIGEST, rows, |row| {
        Ok(WidgetCreateReplayRow {
            canonical_command: row.decode("canonical_command")?,
            code: row.decode("code")?,
            created_at: row.decode("created_at")?,
            edit_version: row.decode("edit_version")?,
            id: row.decode("id")?,
            maker_id: row.decode("maker_id")?,
            note: row.decode("note")?,
        })
    })
}

pub(crate) async fn create(
    mut claim: PendingClaim,
    id: wamn_postgres_statements::Uuid,
    code: String,
    maker_id: Option<wamn_postgres_statements::Uuid>,
    note: Option<String>,
) -> Result<FinalizedClaim, wamn_postgres_statements::StatementError> {
    let rows = claim
        .transaction
        .run(
            CREATE_2_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(code),
                wamn_postgres_statements::into_sql_value(maker_id),
                wamn_postgres_statements::into_sql_value(note),
            ],
        )
        .await?;
    let row = wamn_postgres_statements::decode_one(CREATE_2_DIGEST, rows, |row| {
        Ok(WidgetRow {
            code: row.decode("code")?,
            created_at: row.decode("created_at")?,
            edit_version: row.decode("edit_version")?,
            id: row.decode("id")?,
            maker_id: row.decode("maker_id")?,
            note: row.decode("note")?,
        })
    })?;
    Ok(FinalizedClaim {
        transaction: claim.transaction,
        row,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the parameters are the statement's bind list"
)]
pub(crate) async fn update(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
    expected_edit_version: i64,
    code_present: bool,
    code_value: Option<String>,
    maker_id_present: bool,
    maker_id_value: Option<wamn_postgres_statements::Uuid>,
    note_present: bool,
    note_value: Option<String>,
) -> Result<WidgetUpdateRow, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            UPDATE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(expected_edit_version),
                wamn_postgres_statements::into_sql_value(code_present),
                wamn_postgres_statements::into_sql_value(code_value),
                wamn_postgres_statements::into_sql_value(maker_id_present),
                wamn_postgres_statements::into_sql_value(maker_id_value),
                wamn_postgres_statements::into_sql_value(note_present),
                wamn_postgres_statements::into_sql_value(note_value),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(UPDATE_DIGEST, rows, |row| {
        Ok(WidgetUpdateRow {
            outcome: row.decode("outcome")?,
            observed_edit_version: row.decode("observed_edit_version")?,
            code: row.decode("code")?,
            created_at: row.decode("created_at")?,
            edit_version: row.decode("edit_version")?,
            id: row.decode("id")?,
            maker_id: row.decode("maker_id")?,
            note: row.decode("note")?,
        })
    })
}

pub(crate) async fn delete(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
    expected_edit_version: i64,
) -> Result<WidgetDeleteRow, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            DELETE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(id),
                wamn_postgres_statements::into_sql_value(expected_edit_version),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(DELETE_DIGEST, rows, |row| {
        Ok(WidgetDeleteRow {
            outcome: row.decode("outcome")?,
        })
    })
}
