SELECT
    location_id,
    row_version,
    status
FROM packaging
WHERE id = $1
FOR UPDATE;
