SELECT
    model.code,
    model.created_at,
    model.edit_version,
    model.id,
    model.maker_id,
    model.note
FROM widget AS model
WHERE model.id = $1::uuid;
