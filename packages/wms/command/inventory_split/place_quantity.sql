INSERT INTO pallet_quantity (pallet_id, product_id, status, quantity)
VALUES ($1, $2, $3, $4)
RETURNING id, quantity;
