//! Compile the supported claim lifecycle with real generated accessors.

mod receiving {
    include!(env!("WAMN_SPLIT_PROBE_ACCESSORS"));
}

mod wms {
    include!(env!("WAMN_SPLIT_PROBE_WMS_ACCESSORS"));
}

use wamn_postgres_statements::{Connection, StatementError, TimestampTz, Uuid};

/// Keep the Receiving claim and receipt in the owned transaction.
pub async fn receiving_claim(
    connection: &mut Connection,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    purchase_order_id: Uuid,
    occurred_at: TimestampTz,
) -> Result<(), StatementError> {
    let mut pending = receiving::begin_claim(connection.begin().await?);
    let Some(claim) = receiving::claim_command(
        &mut pending,
        idempotency_key.clone(),
        canonical_command.clone(),
        purchase_order_id.clone(),
    )
    .await?
    else {
        return Ok(());
    };
    let receipt_id = claim.receipt_id.clone();
    receiving::insert_receipt(
        &mut pending,
        receipt_id.clone(),
        idempotency_key.clone(),
        purchase_order_id,
        String::from("owning-claim-probe"),
        occurred_at,
    )
    .await?;
    let finalized = receiving::finalize_command(
        pending,
        idempotency_key,
        canonical_command,
        receipt_id,
        String::from("received"),
        1,
    )
    .await?;
    let _row_version = finalized.row.row_version;
    finalized.commit().await
}

/// Keep both WMS split identities in the owned transaction.
pub async fn wms_split_claim(
    connection: &mut Connection,
    idempotency_key: String,
    canonical_command: Vec<u8>,
    source_pallet_id: Uuid,
    location_id: Uuid,
) -> Result<(), StatementError> {
    let mut pending = wms::begin_claim(connection.begin().await?);
    let Some(claim) = wms::claim_command(
        &mut pending,
        idempotency_key.clone(),
        canonical_command.clone(),
        source_pallet_id,
    )
    .await?
    else {
        return Ok(());
    };
    let movement_id = claim.movement_id.clone();
    let new_pallet_id = claim.new_pallet_id.clone();
    let pallet = wms::create_pallet(
        &mut pending,
        new_pallet_id,
        String::from("owning-claim-probe"),
        location_id,
        String::from("available"),
    )
    .await?;
    let finalized = wms::finalize_command(
        pending,
        idempotency_key,
        canonical_command,
        movement_id,
        pallet.row_version,
    )
    .await?;
    let _row_version = finalized.row.row_version;
    finalized.commit().await
}
