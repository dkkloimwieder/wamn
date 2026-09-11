SELECT
    location_id,
    row_version,
    status
FROM pallet
WHERE id = $1
FOR UPDATE;
