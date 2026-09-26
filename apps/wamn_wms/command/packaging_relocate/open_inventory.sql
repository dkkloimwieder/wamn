SELECT id FROM inventory WHERE packaging_id = $1 AND lifecycle = 'open' ORDER BY id;
