SELECT
    claim.canonical_command,
    model.created_at,
    model.id,
    model.product_code,
    model.row_version
FROM product_command AS claim
JOIN product AS model
    ON model.id = claim.product_id
WHERE claim.idempotency_key = $1::text;
