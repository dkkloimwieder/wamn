-- The whole source moved to the target, so its balance rows are gone.
DELETE FROM packaging_quantity
WHERE packaging_id = $1
RETURNING id;
