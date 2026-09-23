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
        $2::timestamptz IS NULL
        OR widget.created_at > $2::timestamptz
        OR (
            widget.created_at = $2::timestamptz
            AND widget.id > $3::uuid
        )
    )
ORDER BY
    widget.created_at ASC,
    widget.id ASC
LIMIT $4::int8;
