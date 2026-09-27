INSERT INTO location (location_code)
VALUES ($1::text)
RETURNING
    created_at,
    id,
    location_code,
    row_version;
