SELECT id, type, code, location_id, lifecycle, row_version FROM packaging WHERE id = $1 FOR UPDATE;
