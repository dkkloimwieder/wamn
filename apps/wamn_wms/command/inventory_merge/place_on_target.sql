INSERT INTO packaging_quantity (packaging_id, product_id, status, quantity)
VALUES ($1, $2, $3, $4)
RETURNING id, quantity;
