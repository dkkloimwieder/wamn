INSERT INTO pallet (pallet_code, location_id, status)
VALUES ($1::text, $2::uuid, $3::text)
RETURNING
    created_at,
    created_by,
    id,
    location_id,
    pallet_code,
    row_version,
    status,
    updated_at,
    updated_by;
