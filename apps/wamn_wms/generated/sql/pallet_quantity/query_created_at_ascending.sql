SELECT
    model.created_at,
    model.id,
    model.pallet_id,
    model.product_id,
    model.quantity,
    model.status
FROM pallet_quantity AS model
WHERE
    ($1::timestamptz IS NULL OR model.created_at > $1::timestamptz
        OR (model.created_at = $1::timestamptz AND model.id > $2::uuid))
ORDER BY model.created_at ASC, model.id ASC
LIMIT $3::int8;
