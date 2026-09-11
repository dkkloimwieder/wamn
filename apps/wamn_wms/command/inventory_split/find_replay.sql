SELECT
    canonical_command,
    movement_id,
    source_pallet_id,
    new_pallet_id,
    row_version
FROM inventory_split_command
WHERE idempotency_key = $1;
