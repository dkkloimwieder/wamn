SELECT
    model.created_at,
    model.created_by,
    model.from_location_id,
    model.id,
    model.idempotency_key,
    model.kind,
    model.occurred_at,
    model.pallet_id,
    model.product_id,
    model.quantity,
    model.reason_code,
    model.to_location_id
FROM inventory_movement AS model
WHERE
    ($1::timestamptz IS NULL OR model.created_at > $1::timestamptz
        OR (model.created_at = $1::timestamptz AND model.id > $2::uuid))
ORDER BY model.created_at ASC, model.id ASC
LIMIT $3::int8;
