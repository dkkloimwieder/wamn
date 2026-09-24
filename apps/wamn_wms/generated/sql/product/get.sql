SELECT
    model.created_at,
    model.id,
    model.product_code,
    model.row_version
FROM product AS model
WHERE model.id = $1::uuid;
