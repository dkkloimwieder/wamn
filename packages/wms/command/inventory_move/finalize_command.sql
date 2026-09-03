UPDATE inventory_move_command
SET
    pallet_status = $4,
    row_version = $5
WHERE idempotency_key = $1
    AND canonical_command = $2
    AND movement_id = $3
    AND pallet_status IS NULL
    AND row_version IS NULL
RETURNING pallet_status, row_version;
