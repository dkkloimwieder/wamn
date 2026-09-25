SELECT
    model.code,
    model.created_at,
    model.id,
    model.lifecycle,
    model.location_id,
    model.row_version,
    model.type
FROM packaging AS model
WHERE
    ($1::timestamptz IS NULL OR model.created_at > $1::timestamptz
        OR (model.created_at = $1::timestamptz AND model.id > $2::uuid))
ORDER BY model.created_at ASC, model.id ASC
LIMIT $3::int8;
