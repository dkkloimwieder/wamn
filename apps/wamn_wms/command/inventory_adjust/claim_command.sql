INSERT INTO inventory_adjust_command (
    idempotency_key,
    canonical_command,
    pallet_id
)
VALUES ($1, $2, $3)
ON CONFLICT ON CONSTRAINT inventory_adjust_command_idempotency_key_pkey
DO NOTHING
RETURNING movement_id;
