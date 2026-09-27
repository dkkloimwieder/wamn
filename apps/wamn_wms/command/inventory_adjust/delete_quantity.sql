-- A balance row means stock is present, so a count of zero removes it.
DELETE FROM packaging_quantity
WHERE packaging_id = $1
    AND product_id = $2
    AND status = $3
RETURNING id;
