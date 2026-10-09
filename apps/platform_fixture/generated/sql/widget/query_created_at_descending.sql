SELECT
    model.code,
    model.created_at,
    model.edit_version,
    model.id,
    model.maker_id,
    model.note
FROM widget AS model
WHERE
    ($1::jsonb IS NULL OR model.code IN (
        SELECT filter.value::text
        FROM jsonb_array_elements_text($1::jsonb) AS filter(value)
    ))
    AND
    ($2::jsonb IS NULL OR EXISTS (
        SELECT 1
        FROM jsonb_array_elements_text($2::jsonb) AS filter(value)
        WHERE starts_with(model.note, filter.value)
    ))
    AND
    ($3::jsonb IS NULL OR (model.maker_id IS NULL) = ($3::jsonb)::boolean)
    AND
    ($4::timestamptz IS NULL OR model.created_at < $4::timestamptz
        OR (model.created_at = $4::timestamptz AND model.id < $5::uuid))
ORDER BY model.created_at DESC, model.id DESC
LIMIT $6::int8;
