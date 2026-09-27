// @generated from migration IR; do not edit.

use wamn_postgres_statements::Transaction;

#[derive(Debug)]
pub struct RecordSampleRow {
    pub id: wamn_postgres_statements::Uuid,
}

pub(crate) const RECORD_SAMPLE_DIGEST: &str =
    "sha256:02043aaa0b79b5dda61b14d75a007a631a385db74e13d413017e9126cfc531fe";

pub(crate) async fn record_sample(
    transaction: &mut Transaction,
    frame: String,
    captured_at: wamn_postgres_statements::TimestampTz,
) -> Result<RecordSampleRow, wamn_postgres_statements::StatementError> {
    let rows = transaction
        .run(
            RECORD_SAMPLE_DIGEST,
            vec![
                wamn_postgres_statements::into_sql_value(frame),
                wamn_postgres_statements::into_sql_value(captured_at),
            ],
        )
        .await?;
    wamn_postgres_statements::decode_one(RECORD_SAMPLE_DIGEST, rows, |row| {
        Ok(RecordSampleRow {
            id: row.decode("id")?,
        })
    })
}
