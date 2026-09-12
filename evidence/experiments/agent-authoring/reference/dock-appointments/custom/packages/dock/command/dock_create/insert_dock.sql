INSERT INTO dock (
    id,
    name
)
VALUES ($1, $2)
RETURNING id;
