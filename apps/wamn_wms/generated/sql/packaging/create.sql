INSERT INTO packaging (packaging_code, type, location_id, status)
VALUES ($1::text, $2::text, $3::uuid, $4::text)
RETURNING
    created_at,
    created_by,
    id,
    location_id,
    packaging_code,
    row_version,
    status,
    type,
    updated_at,
    updated_by;
