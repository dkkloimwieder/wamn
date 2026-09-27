UPDATE packaging_quantity
SET quantity = $4
WHERE packaging_id = $1
    AND product_id = $2
    AND status = $3
RETURNING id, quantity;
