// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct LockPackagingRow {
    pub location_id: wamn_postgres_statements::Uuid,
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug)]
pub struct MovePackagingRow {
    pub location_id: wamn_postgres_statements::Uuid,
    pub row_version: i32,
    pub status: String,
}

#[derive(Debug)]
pub struct ValidateLocationRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const LOCK_PACKAGING_DIGEST: &str =
    "sha256:404783208c619d13cc5921442a94d7698cf002213e042b8db230086980a7c84a";
pub(crate) const MOVE_PACKAGING_DIGEST: &str =
    "sha256:3c966597d9a67570931537e4ac8427310ed818fc30bcc97d94419d7f791a1099";
pub(crate) const VALIDATE_LOCATION_DIGEST: &str =
    "sha256:043f1cb7e8359f79c83b7944e308c1d4238a2bc7b0eac50a0093e53e7563d516";

pub(crate) async fn lock_packaging(
    transaction: &mut Transaction,
    packaging_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LockPackagingRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            LOCK_PACKAGING_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(packaging_id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(LOCK_PACKAGING_DIGEST, rows, |row| {
        Ok(LockPackagingRow {
            location_id: row.decode("location_id")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn move_packaging(
    transaction: &mut Transaction,
    packaging_id: wamn_postgres_statements::Uuid,
    to_location_id: wamn_postgres_statements::Uuid,
) -> Result<MovePackagingRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            MOVE_PACKAGING_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(packaging_id),
                wamn_postgres_statements::into_sql_value(to_location_id),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(MOVE_PACKAGING_DIGEST, rows, |row| {
        Ok(MovePackagingRow {
            location_id: row.decode("location_id")?,
            row_version: row.decode("row_version")?,
            status: row.decode("status")?,
        })
    })
}

pub(crate) async fn validate_location(
    transaction: &mut Transaction,
    location_id: wamn_postgres_statements::Uuid,
) -> Result<Option<ValidateLocationRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction
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
