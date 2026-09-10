// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct ListLocationsRow {
    pub id: wamn_postgres_statements::Uuid,
    pub location_code: String,
}

pub(crate) const LIST_LOCATIONS_DIGEST: &str = "sha256:35923fd698b2742abe220aeb88e5318ddf8e9fd3adbb56a913d07dc01957e38a";

pub(crate) async fn list_locations(
    transaction: &mut Transaction,
) -> Result<Vec<ListLocationsRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(LIST_LOCATIONS_DIGEST, vec![
    ]).await?;
    wamn_postgres_statements::decode_all(LIST_LOCATIONS_DIGEST, rows, |row| {
        Ok(ListLocationsRow {
            id: row.decode("id")?,
            location_code: row.decode("location_code")?,
        })
    })
}
