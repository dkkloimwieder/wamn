SELECT
    model.created_at,
    model.id,
    model.location_id,
    model.pallet_code,
    model.row_version,
    model.status,
    model.updated_at
FROM pallet AS model
WHERE model.id = $1::uuid;
