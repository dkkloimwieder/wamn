// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct ArchiveRow {
    pub id: wamn_postgres_statements::Uuid,
    pub edit_version: i64,
}

pub(crate) const ARCHIVE_DIGEST: &str =
    "sha256:60d573f8af069d58f1a420ac844eb8a7ae81f9f18a452ca5b03f19f301cf0b99";

pub(crate) async fn archive(
    transaction: &mut Transaction,
    id: wamn_postgres_statements::Uuid,
) -> Result<ArchiveRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            ARCHIVE_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(id)],
        )
        .await?;
    wamn_postgres_statements::decode_one(ARCHIVE_DIGEST, rows, |row| {
        Ok(ArchiveRow {
            id: row.decode("id")?,
            edit_version: row.decode("edit_version")?,
        })
    })
}
