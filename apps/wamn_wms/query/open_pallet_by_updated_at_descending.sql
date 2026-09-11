SELECT
    pallet.created_at,
    pallet.id,
    pallet.location_id,
    pallet.pallet_code,
    pallet.row_version,
    pallet.status,
    pallet.updated_at
FROM pallet AS pallet
WHERE
    (
        $1::jsonb IS NULL
        OR pallet.status IN (
            SELECT filter.value
            FROM jsonb_array_elements_text($1::jsonb) AS filter(value)
        )
    )
    AND (
        $2::jsonb IS NULL
        OR pallet.location_id::text IN (
            SELECT filter.value
            FROM jsonb_array_elements_text($2::jsonb) AS filter(value)
        )
    )
    AND (
        $3::jsonb IS NULL
        OR pallet.pallet_code IN (
            SELECT filter.value
            FROM jsonb_array_elements_text($3::jsonb) AS filter(value)
        )
    )
    AND (
        $4::timestamptz IS NULL
        OR pallet.updated_at < $4::timestamptz
        OR (
            pallet.updated_at = $4::timestamptz
            AND pallet.id > $5::uuid
        )
    )
ORDER BY
    pallet.updated_at DESC,
    pallet.id ASC
LIMIT $6::int8;
