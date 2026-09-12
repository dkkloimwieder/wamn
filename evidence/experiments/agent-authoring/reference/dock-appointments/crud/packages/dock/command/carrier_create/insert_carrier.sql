INSERT INTO carrier (
    id,
    name
)
VALUES ($1, $2)
RETURNING id;
