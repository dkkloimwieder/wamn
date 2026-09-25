SELECT
    model.created_at,
    model.edit_version,
    model.id,
    model.name
FROM widget_maker AS model
WHERE model.id = $1::uuid;
