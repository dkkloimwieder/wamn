SELECT quantity
FROM packaging_quantity
WHERE packaging_id = $1
    AND product_id = $2
    AND status = $3;
