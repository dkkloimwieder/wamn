INSERT INTO location (location_code)
VALUES ($1::text)
RETURNING
    created_at,
    description,
    id,
    location_code,
    row_version;
