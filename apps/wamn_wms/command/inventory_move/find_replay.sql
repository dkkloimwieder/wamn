SELECT
    canonical_command,
    movement_id,
    pallet_id,
    pallet_status,
    row_version
FROM inventory_move_command
WHERE idempotency_key = $1;
