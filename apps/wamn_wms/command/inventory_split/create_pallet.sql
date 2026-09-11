INSERT INTO pallet (id, pallet_code, location_id, status)
VALUES ($1, $2, $3, $4)
RETURNING id, row_version, status;
