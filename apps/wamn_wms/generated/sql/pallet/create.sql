INSERT INTO pallet (id, pallet_code, location_id, status)
VALUES ($1::uuid, $2::text, $3::uuid, $4::text)
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
