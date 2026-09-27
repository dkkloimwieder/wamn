SELECT
    model.created_at,
    model.created_by,
    model.id,
    model.location_id,
    model.packaging_code,
    model.row_version,
    model.status,
    model.type,
    model.updated_at,
    model.updated_by
FROM packaging AS model
WHERE model.id = $1::uuid;
