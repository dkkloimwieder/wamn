SELECT id, product_id, packaging_id, location_id, quantity, disposition, lifecycle, row_version FROM inventory WHERE packaging_id = $1 AND lifecycle = 'open' ORDER BY id FOR UPDATE;
