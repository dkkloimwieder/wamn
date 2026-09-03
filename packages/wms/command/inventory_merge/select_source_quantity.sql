SELECT
    product_id,
    quantity,
    status
FROM pallet_quantity
WHERE pallet_id = $1
ORDER BY product_id ASC, status ASC;
