// @generated from migration IR; do not edit.

use wamn_postgres_statements::TransactionView;

#[derive(Debug)]
pub struct RecordReceiptParticipantRow {
    pub outcome: Option<String>,
    pub receipt_id: Option<wamn_postgres_statements::Uuid>,
}

pub(crate) const RECORD_RECEIPT_PARTICIPANT_DIGEST: &str =
    "sha256:964d38a35353ea399a15e73286c137c32a510457c2e1d9a60897592d576edc85";

pub(crate) async fn record_receipt_participant(
    transaction: &mut TransactionView,
    receipt_id: wamn_postgres_statements::Uuid,
    purchase_order_id: wamn_postgres_statements::Uuid,
) -> Result<RecordReceiptParticipantRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            RECORD_RECEIPT_PARTICIPANT_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(receipt_id),
                wamn_postgres_statements::into_sql_value(purchase_order_id),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(RECORD_RECEIPT_PARTICIPANT_DIGEST, rows, |row| {
        Ok(RecordReceiptParticipantRow {
            outcome: row.decode("outcome")?,
            receipt_id: row.decode("receipt_id")?,
        })
    })
}
