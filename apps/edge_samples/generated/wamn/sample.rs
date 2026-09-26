// @generated from migration IR; do not edit.

use wamn_postgres_statements::Connection;

#[derive(Debug)]
pub struct SampleRow {
    pub captured_at: wamn_postgres_statements::TimestampTz,
    pub created_at: wamn_postgres_statements::TimestampTz,
    pub frame: String,
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const GET_DIGEST: &str =
    "sha256:6f50fb94d0cdc186aad232771b508f81633779d8f54fcf502d87b26cfc4da2f4";

pub(crate) async fn get(
    connection: &mut Connection,
    id: wamn_postgres_statements::Uuid,
) -> Result<Option<SampleRow>, wamn_postgres_statements::StatementError> {
    let rows = connection
        .run(
            GET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_optional(GET_DIGEST, rows, |row| {
        Ok(SampleRow {
            captured_at: row.decode("captured_at")?,
            created_at: row.decode("created_at")?,
            frame: row.decode("frame")?,
            id: row.decode("id")?,
        })
    })
}
