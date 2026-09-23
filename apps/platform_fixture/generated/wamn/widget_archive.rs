// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct ArchiveRow {
    pub id: wamn_postgres_statements::Uuid,
    pub edit_version: i64,
    pub note: Option<String>,
}

pub(crate) const ARCHIVE_DIGEST: &str =
    "sha256:340afea3740efd68e6c9b3925765fa40766b352562bb78f39c2c1973dac3fedf";

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
            note: row.decode("note")?,
        })
    })
}
