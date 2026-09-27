SELECT
    product_id,
    quantity,
    status
FROM packaging_quantity
WHERE packaging_id = $1
ORDER BY product_id ASC, status ASC;
