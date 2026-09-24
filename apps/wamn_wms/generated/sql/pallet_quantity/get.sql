SELECT
    model.created_at,
    model.id,
    model.pallet_id,
    model.product_id,
    model.quantity,
    model.status
FROM pallet_quantity AS model
WHERE model.id = $1::uuid;
