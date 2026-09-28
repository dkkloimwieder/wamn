INSERT INTO packaging (packaging_code, type, location_id, located_at, status)
VALUES ($1, $2, $3, $4, $5)
RETURNING id, row_version, status;
