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
WHERE model.id = $1::uuid;
