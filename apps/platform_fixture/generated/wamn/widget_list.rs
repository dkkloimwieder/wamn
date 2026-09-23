// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct ListRow {
    pub id: wamn_postgres_statements::Uuid,
    pub code: String,
    pub edit_version: i64,
    pub attributes: wamn_postgres_statements::Json,
}

pub(crate) const LIST_DIGEST: &str =
    "sha256:33acdf89c4eb1ec948b55ae1b4b827d81ef0415ff375bc2a7b73ffa7614c5ef2";

pub(crate) async fn list(
    transaction: &mut Transaction,
) -> Result<Vec<ListRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(LIST_DIGEST, vec![]).await?;
    wamn_postgres_statements::decode_all(LIST_DIGEST, rows, |row| {
        Ok(ListRow {
            id: row.decode("id")?,
            code: row.decode("code")?,
            edit_version: row.decode("edit_version")?,
            attributes: row.decode("attributes")?,
        })
    })
}
