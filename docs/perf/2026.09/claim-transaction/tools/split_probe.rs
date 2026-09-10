//! Compile probe for claim and row writes in separate transactions.

mod generated {
    include!(env!("WAMN_SPLIT_PROBE_ACCESSORS"));
}

use wamn_postgres_statements::{Connection, StatementError, TimestampTz, Uuid};

/// Compile the split shape through the shipped generated accessors.
pub async fn split_claim_and_row(
    connection: &mut Connection,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    purchase_order_id: Uuid,
    occurred_at: TimestampTz,
) -> Result<(), StatementError> {
    let mut claim_transaction = connection.begin().await?;
    let claim = generated::claim_command(
        &mut claim_transaction,
        idempotency_key.clone(),
        canonical_command.clone(),
        purchase_order_id.clone(),
    )
    .await?;
    claim_transaction.commit().await?;

    if let Some(claim) = claim {
        let mut row_transaction = connection.begin().await?;
        generated::insert_receipt(
            &mut row_transaction,
            claim.receipt_id.clone(),
            idempotency_key.clone(),
            purchase_order_id,
            String::from("split-transaction-probe"),
            occurred_at,
        )
        .await?;
        generated::finalize_command(
            &mut row_transaction,
            idempotency_key,
            canonical_command,
            claim.receipt_id,
            String::from("received"),
            1,
        )
        .await?;
        row_transaction.commit().await?;
    }
    Ok(())
}
