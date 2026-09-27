SELECT
    packaging.created_at,
    packaging.created_by,
    packaging.id,
    packaging.location_id,
    packaging.packaging_code,
    packaging.row_version,
    packaging.status,
    packaging.type,
    packaging.updated_at,
    packaging.updated_by
FROM packaging AS packaging
WHERE
    (
        $1::jsonb IS NULL
        OR packaging.status IN (
            SELECT filter.value
            FROM jsonb_array_elements_text($1::jsonb) AS filter(value)
        )
    )
    AND (
        $2::jsonb IS NULL
        OR packaging.location_id::text IN (
            SELECT filter.value
            FROM jsonb_array_elements_text($2::jsonb) AS filter(value)
        )
    )
    AND (
        $3::jsonb IS NULL
        OR EXISTS (
            SELECT 1
            FROM jsonb_array_elements_text($3::jsonb) AS filter(value)
            WHERE strpos(packaging.packaging_code, filter.value) > 0
        )
    )
    AND (
        $4::uuid IS NULL
        OR packaging.location_id < $4::uuid
        OR (
            packaging.location_id = $4::uuid
            AND packaging.id > $5::uuid
        )
    )
ORDER BY
    packaging.location_id DESC,
    packaging.id ASC
LIMIT $6::int8;
