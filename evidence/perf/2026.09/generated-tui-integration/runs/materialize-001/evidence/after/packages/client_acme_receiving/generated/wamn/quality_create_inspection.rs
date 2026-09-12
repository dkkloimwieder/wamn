// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub(crate) struct InsertInspectionRow {
    pub receipt_id: wamn_postgres_statements::Uuid,
}

#[derive(Debug)]
pub(crate) struct LoadInspectionRow {
    pub receipt_id: wamn_postgres_statements::Uuid,
}

pub(crate) const INSERT_INSPECTION_DIGEST: &str = "sha256:5e18c52dedc3857274c7f6a246a1e486b50d4afebff4081edebd3232ceadba14";
pub(crate) const LOAD_INSPECTION_DIGEST: &str = "sha256:0f21db223ec017fa15f04c26dd646b85270965bda939885c2195674960e9242a";

pub(crate) async fn insert_inspection(
    transaction: &mut Transaction,
    receipt_id: wamn_postgres_statements::Uuid,
) -> Result<Option<InsertInspectionRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(INSERT_INSPECTION_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(receipt_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(INSERT_INSPECTION_DIGEST, rows, |row| {
        Ok(InsertInspectionRow {
            receipt_id: row.decode("receipt_id")?,
        })
    })
}

pub(crate) async fn load_inspection(
    transaction: &mut Transaction,
    receipt_id: wamn_postgres_statements::Uuid,
) -> Result<Option<LoadInspectionRow>, wamn_postgres_statements::StatementError> {
    let rows = transaction.run(LOAD_INSPECTION_DIGEST, vec![
        wamn_postgres_statements::into_sql_value(receipt_id),
    ]).await?;
    wamn_postgres_statements::decode_optional(LOAD_INSPECTION_DIGEST, rows, |row| {
        Ok(LoadInspectionRow {
            receipt_id: row.decode("receipt_id")?,
        })
    })
}
