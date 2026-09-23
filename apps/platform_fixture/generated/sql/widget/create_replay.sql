SELECT
    claim.canonical_command,
    model.code,
    model.created_at,
    model.edit_version,
    model.id,
    model.maker_id,
    model.note
FROM widget_command AS claim
JOIN widget AS model
    ON model.id = claim.widget_id
WHERE claim.idempotency_key = $1::text;
