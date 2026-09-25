SELECT id, type, code, location_id, lifecycle, row_version FROM packaging WHERE id IN ($1, $2) ORDER BY id FOR UPDATE;
