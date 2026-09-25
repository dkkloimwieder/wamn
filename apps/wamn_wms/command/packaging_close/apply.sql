UPDATE packaging SET lifecycle = 'closed', row_version = row_version + 1 WHERE id = $1 RETURNING id, type, code, location_id, lifecycle, row_version;
