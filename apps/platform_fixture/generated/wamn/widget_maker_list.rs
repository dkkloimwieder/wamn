// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct ListRow {
    pub id: wamn_postgres_statements::Uuid,
    pub name: String,
}

pub(crate) const LIST_DIGEST: &str =
    "sha256:b3b8e7bc0d91079eedccc6097de8a5d50abd9070924ae5771cd3c17b8fee7d67";

pub(crate) async fn list(
    transaction: &mut Transaction,
) -> Result<Vec<ListRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(LIST_DIGEST, vec![]).await?;
    wamn_postgres_statements::decode_all(LIST_DIGEST, rows, |row| {
        Ok(ListRow {
            id: row.decode("id")?,
            name: row.decode("name")?,
        })
    })
}
