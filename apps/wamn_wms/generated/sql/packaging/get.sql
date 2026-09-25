SELECT
    model.code,
    model.created_at,
    model.id,
    model.lifecycle,
    model.location_id,
    model.row_version,
    model.type
FROM packaging AS model
WHERE model.id = $1::uuid;
