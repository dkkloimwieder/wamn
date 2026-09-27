INSERT INTO packaging (packaging_code, type, location_id, status)
VALUES ($1, $2, $3, $4)
RETURNING id, row_version, status;
