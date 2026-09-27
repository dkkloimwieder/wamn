SELECT
    model.created_at,
    model.id,
    model.packaging_id,
    model.product_id,
    model.quantity,
    model.status
FROM packaging_quantity AS model
WHERE model.id = $1::uuid;
