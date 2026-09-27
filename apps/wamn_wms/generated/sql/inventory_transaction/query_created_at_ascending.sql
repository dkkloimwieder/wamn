SELECT
    model.created_at,
    model.created_by,
    model.from_packaging_id,
    model.from_status,
    model.id,
    model.occurred_at,
    model.product_id,
    model.quantity,
    model.reason_code,
    model.to_packaging_id,
    model.to_status
FROM inventory_transaction AS model
WHERE
    ($1::timestamptz IS NULL OR model.created_at > $1::timestamptz
        OR (model.created_at = $1::timestamptz AND model.id > $2::uuid))
ORDER BY model.created_at ASC, model.id ASC
LIMIT $3::int8;
