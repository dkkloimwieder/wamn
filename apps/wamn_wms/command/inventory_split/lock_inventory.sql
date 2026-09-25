SELECT id, product_id, packaging_id, location_id, quantity, disposition, lifecycle, row_version FROM inventory WHERE id = $1 FOR UPDATE;
