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
WHERE model.id = $1::uuid;
