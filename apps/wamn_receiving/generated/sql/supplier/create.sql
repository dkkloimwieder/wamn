INSERT INTO supplier (name)
VALUES ($1::text)
RETURNING
    created_at,
    id,
    name;
