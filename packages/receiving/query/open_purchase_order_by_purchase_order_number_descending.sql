SELECT
    purchase_order.id,
    purchase_order.purchase_order_number,
    purchase_order.supplier_id,
    purchase_order.status,
    purchase_order.row_version,
    purchase_order.created_at,
    purchase_order.updated_at
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
        $3::text IS NULL
        OR purchase_order.purchase_order_number < $3::text
        OR (
            purchase_order.purchase_order_number = $3::text
            AND purchase_order.id < $4::uuid
        )
    )
ORDER BY
    purchase_order.purchase_order_number DESC,
    purchase_order.id DESC
LIMIT $5::int8;
