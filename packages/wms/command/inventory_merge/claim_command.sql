INSERT INTO inventory_merge_command (
    idempotency_key,
    canonical_command,
    source_pallet_id,
    target_pallet_id
)
VALUES ($1, $2, $3, $4)
ON CONFLICT ON CONSTRAINT inventory_merge_command_idempotency_key_pkey
DO NOTHING
RETURNING movement_id;
