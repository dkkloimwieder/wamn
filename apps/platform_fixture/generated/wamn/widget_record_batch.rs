// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct FindWidgetRow {
    pub widget_id: wamn_postgres_statements::Uuid,
}

pub(crate) const FIND_WIDGET_DIGEST: &str =
    "sha256:1a9e1df64bfca27b246fae9766cd571b8c6dc079ce30b4894cac6fe4375b73fe";

pub(crate) async fn find_widget(
    transaction: &mut Transaction,
    widget_id: wamn_postgres_statements::Uuid,
) -> Result<FindWidgetRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            FIND_WIDGET_DIGEST,
            vec![wamn_postgres_statements::into_sql_value(widget_id)],
        )
        .await?;
    wamn_postgres_statements::decode_one(FIND_WIDGET_DIGEST, rows, |row| {
        Ok(FindWidgetRow {
            widget_id: row.decode("widget_id")?,
        })
    })
}
