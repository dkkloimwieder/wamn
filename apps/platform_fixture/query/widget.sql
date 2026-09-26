SELECT
    widget.code,
    widget.created_at,
    widget.edit_version,
    widget.id,
    widget.maker_id,
    widget.note
FROM widget AS widget
WHERE
    (
        $1::jsonb IS NULL
        OR widget.code IN (
            SELECT filter.value
            FROM jsonb_array_elements_text($1::jsonb) AS filter(value)
        )
    )
    AND (
        $2::jsonb IS NULL
        OR EXISTS (
            SELECT 1
            FROM jsonb_array_elements_text($2::jsonb) AS filter(value)
            WHERE starts_with(widget.note, filter.value)
        )
    )
    AND (
        $3::jsonb IS NULL
        OR (widget.maker_id IS NULL) = ($3::jsonb)::boolean
    )
    AND (
        $4::timestamptz IS NULL
        OR widget.created_at > $4::timestamptz
        OR (
            widget.created_at = $4::timestamptz
            AND widget.id > $5::uuid
        )
    )
ORDER BY
    widget.created_at ASC,
    widget.id ASC
LIMIT $6::int8;
