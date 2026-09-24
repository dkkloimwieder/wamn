INSERT INTO location (id, location_code)
VALUES ($1::uuid, $2::text)
RETURNING
    created_at,
    id,
    location_code,
    row_version;
