// @generated from migration IR; do not edit.

#[derive(Debug, sqlx::FromRow)]
pub struct RecordReceiptParticipantRow {
    pub outcome: Option<String>,
    pub receipt_id: Option<uuid::Uuid>,
}

pub(crate) const RECORD_RECEIPT_PARTICIPANT_SQL: &str =
    include_str!("../../command/record_receipt_participant/record_receipt_participant.sql");

pub(crate) fn record_receipt_participant_receipt_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
pub(crate) fn record_receipt_participant_purchase_order_id_bind_fixture() -> uuid::Uuid {
    uuid::Uuid::nil()
}
