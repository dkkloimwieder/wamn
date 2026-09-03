SELECT
    canonical_command,
    movement_id,
    source_pallet_id,
    target_pallet_id,
    row_version
FROM inventory_merge_command
WHERE idempotency_key = $1;
