SELECT
    claim.canonical_command,
    model.created_at,
    model.id,
    model.name
FROM supplier_command AS claim
JOIN supplier AS model
    ON model.id = claim.supplier_id
WHERE claim.idempotency_key = $1::text;
