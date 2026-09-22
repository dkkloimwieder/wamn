INSERT INTO supplier (id, name)
VALUES ($1::uuid, $2::text)
RETURNING
    created_at,
    id,
    name;
