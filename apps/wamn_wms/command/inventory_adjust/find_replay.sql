SELECT
    canonical_command,
    movement_id,
    pallet_id,
    adjusted_quantity,
    row_version
FROM inventory_adjust_command
WHERE idempotency_key = $1;
