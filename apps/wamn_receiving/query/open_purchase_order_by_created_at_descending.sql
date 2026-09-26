SELECT
    purchase_order.created_at,
    purchase_order.created_by,
    purchase_order.id,
    purchase_order.purchase_order_number,
    purchase_order.row_version,
    purchase_order.status,
    purchase_order.supplier_id,
    purchase_order.updated_at,
    purchase_order.updated_by
FROM purchase_order AS purchase_order
WHERE
    (
        $1::jsonb IS NULL
        OR purchase_order.supplier_id::text IN (
            SELECT filter.value
            FROM jsonb_array_elements_text($1::jsonb) AS filter(value)
        )
    )
    AND (
        $2::jsonb IS NULL
        OR purchase_order.status IN (
            SELECT filter.value
            FROM jsonb_array_elements_text($2::jsonb) AS filter(value)
        )
    )
    AND (
        $3::jsonb IS NULL
        OR EXISTS (
            SELECT 1
            FROM jsonb_array_elements_text($3::jsonb) AS filter(value)
            WHERE strpos(purchase_order.purchase_order_number, filter.value) > 0
        )
    )
    AND (
        $4::timestamptz IS NULL
        OR purchase_order.created_at < $4::timestamptz
        OR (
            purchase_order.created_at = $4::timestamptz
            AND purchase_order.id < $5::uuid
        )
    )
ORDER BY
    purchase_order.created_at DESC,
    purchase_order.id DESC
LIMIT $6::int8;
