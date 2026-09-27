INSERT INTO pallet (pallet_code, location_id, status)
VALUES ($1, $2, $3)
RETURNING id, row_version, status;
