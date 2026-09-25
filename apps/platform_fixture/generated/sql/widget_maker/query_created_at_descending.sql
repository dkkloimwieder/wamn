SELECT
    model.created_at,
    model.edit_version,
    model.id,
    model.name
FROM widget_maker AS model
WHERE
    ($1::jsonb IS NULL OR model.name IN (
        SELECT filter.value::text
        FROM jsonb_array_elements_text($1::jsonb) AS filter(value)
    ))
    AND
    ($2::timestamptz IS NULL OR model.created_at < $2::timestamptz
        OR (model.created_at = $2::timestamptz AND model.id < $3::uuid))
ORDER BY model.created_at DESC, model.id DESC
LIMIT $4::int8;
