SELECT
    model.created_at,
    model.edit_version,
    model.id,
    model.name
FROM widget_maker AS model
WHERE
    ($1::jsonb IS NULL OR EXISTS (
        SELECT 1
        FROM jsonb_array_elements_text($1::jsonb) AS filter(value)
        WHERE strpos(model.name, filter.value) > 0
    ))
    AND
    (CASE WHEN $2::jsonb IS NULL
        THEN model.created_at >= now() - make_interval(days => 30)
        ELSE (
        ($2::jsonb->>'min' IS NULL OR model.created_at >= ($2::jsonb->>'min')::timestamptz)
        AND ($2::jsonb->>'max' IS NULL OR model.created_at <= ($2::jsonb->>'max')::timestamptz)
    )
    END)
    AND
    ($3::text IS NULL OR strpos(lower(model.name), lower($3::text)) > 0)
    AND
    ($4::timestamptz IS NULL OR model.created_at > $4::timestamptz
        OR (model.created_at = $4::timestamptz AND model.id > $5::uuid))
ORDER BY model.created_at ASC, model.id ASC
LIMIT $6::int8;
