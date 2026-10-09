SELECT
    model.created_at,
    model.created_by,
    model.id,
    model.located_at,
    model.location_id,
    model.packaging_code,
    model.row_version,
    model.status,
    model.type,
    model.updated_at,
    model.updated_by
FROM packaging AS model
WHERE
    ($1::jsonb IS NULL OR model.status IN (
        SELECT filter.value::text
        FROM jsonb_array_elements_text($1::jsonb) AS filter(value)
    ))
    AND
    ($2::jsonb IS NULL OR model.location_id IN (
        SELECT filter.value::uuid
        FROM jsonb_array_elements_text($2::jsonb) AS filter(value)
    ))
    AND
    ($3::jsonb IS NULL OR EXISTS (
        SELECT 1
        FROM jsonb_array_elements_text($3::jsonb) AS filter(value)
        WHERE strpos(model.packaging_code, filter.value) > 0
    ))
    AND
    ($4::uuid IS NULL OR model.location_id > $4::uuid
        OR (model.location_id = $4::uuid AND model.id > $5::uuid))
ORDER BY model.location_id ASC, model.id ASC
LIMIT $6::int8;
