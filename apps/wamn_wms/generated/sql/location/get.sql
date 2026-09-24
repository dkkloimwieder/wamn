SELECT
    model.created_at,
    model.id,
    model.location_code,
    model.row_version
FROM location AS model
WHERE model.id = $1::uuid;
