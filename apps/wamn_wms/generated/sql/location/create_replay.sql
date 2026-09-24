SELECT
    claim.canonical_command,
    model.created_at,
    model.id,
    model.location_code,
    model.row_version
FROM location_command AS claim
JOIN location AS model
    ON model.id = claim.location_id
WHERE claim.idempotency_key = $1::text;
