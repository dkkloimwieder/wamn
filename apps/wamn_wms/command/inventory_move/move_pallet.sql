UPDATE pallet
SET
    location_id = $2,
    row_version = row_version + 1,
    updated_at = CURRENT_TIMESTAMP
WHERE id = $1
RETURNING location_id, row_version, status;
