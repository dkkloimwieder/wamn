UPDATE packaging SET location_id = $2, row_version = row_version + 1 WHERE id = $1 RETURNING id, type, code, location_id, lifecycle, row_version;
