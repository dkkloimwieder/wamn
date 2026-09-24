SELECT
    claim.canonical_command,
    model.created_at,
    model.created_by,
    model.id,
    model.location_id,
    model.pallet_code,
    model.row_version,
    model.status,
    model.updated_at,
    model.updated_by
FROM pallet_command AS claim
JOIN pallet AS model
    ON model.id = claim.pallet_id
WHERE claim.idempotency_key = $1::text;
