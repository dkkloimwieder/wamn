UPDATE pallet
SET
    row_version = row_version + 1,
    updated_at = CURRENT_TIMESTAMP
WHERE id = $1
RETURNING row_version, status;
