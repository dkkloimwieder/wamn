SELECT
    model.created_at,
    model.disposition,
    model.id,
    model.lifecycle,
    model.location_id,
    model.packaging_id,
    model.product_id,
    model.quantity,
    model.row_version
FROM inventory AS model
WHERE model.id = $1::uuid;
