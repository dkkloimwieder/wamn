SELECT id FROM inventory WHERE packaging_id = $1 AND lifecycle = 'open' LIMIT 1;
