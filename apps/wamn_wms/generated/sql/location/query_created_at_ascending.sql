SELECT
    model.created_at,
    model.id,
    model.location_code,
    model.row_version
FROM location AS model
WHERE
    ($1::jsonb IS NULL OR model.location_code IN (
        SELECT filter.value::text
        FROM jsonb_array_elements_text($1::jsonb) AS filter(value)
    ))
    AND
    ($2::timestamptz IS NULL OR model.created_at > $2::timestamptz
        OR (model.created_at = $2::timestamptz AND model.id > $3::uuid))
ORDER BY model.created_at ASC, model.id ASC
LIMIT $4::int8;
