SELECT
    model.created_at,
    model.created_by,
    model.id,
    model.location_id,
    model.pallet_code,
    model.row_version,
    model.status,
    model.updated_at,
    model.updated_by
FROM pallet AS model
WHERE model.id = $1::uuid;
