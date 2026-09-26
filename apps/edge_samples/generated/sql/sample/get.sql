SELECT
    model.captured_at,
    model.created_at,
    model.frame,
    model.id
FROM sample AS model
WHERE model.id = $1::uuid;
