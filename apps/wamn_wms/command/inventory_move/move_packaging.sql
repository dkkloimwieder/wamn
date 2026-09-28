UPDATE packaging
SET
    location_id = $2,
    located_at = $3,
    row_version = row_version + 1
WHERE id = $1
RETURNING location_id, row_version, status;
