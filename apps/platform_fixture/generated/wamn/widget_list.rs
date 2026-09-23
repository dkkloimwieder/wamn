// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct ListRow {
    pub id: wamn_postgres_statements::Uuid,
    pub code: String,
    pub edit_version: i64,
}

pub(crate) const LIST_DIGEST: &str =
    "sha256:b55457d9adbf517091649a690d88d05d9d32de5f3335bd527ea37545228dcb85";

pub(crate) async fn list(
    transaction: &mut Transaction,
) -> Result<Vec<ListRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(LIST_DIGEST, vec![]).await?;
    wamn_postgres_statements::decode_all(LIST_DIGEST, rows, |row| {
        Ok(ListRow {
            id: row.decode("id")?,
            code: row.decode("code")?,
            edit_version: row.decode("edit_version")?,
        })
    })
}
