UPDATE pallet_quantity
SET quantity = $4
WHERE pallet_id = $1
    AND product_id = $2
    AND status = $3
RETURNING id, quantity;
