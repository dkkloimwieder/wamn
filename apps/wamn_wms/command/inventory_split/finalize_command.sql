UPDATE inventory_split_command
SET row_version = $4
WHERE idempotency_key = $1
    AND canonical_command = $2
    AND movement_id = $3
    AND row_version IS NULL
RETURNING row_version;
