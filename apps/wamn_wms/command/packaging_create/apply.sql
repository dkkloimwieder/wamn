INSERT INTO packaging (id, type, code, location_id, lifecycle) VALUES ($1, $2, $3, $4, 'open') RETURNING id, type, code, location_id, lifecycle, row_version;
