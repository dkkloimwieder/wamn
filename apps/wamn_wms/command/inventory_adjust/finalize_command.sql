UPDATE inventory_adjust_command
SET
    adjusted_quantity = $4,
    row_version = $5
WHERE idempotency_key = $1
    AND canonical_command = $2
    AND movement_id = $3
    AND adjusted_quantity IS NULL
    AND row_version IS NULL
RETURNING adjusted_quantity, row_version;
