INSERT INTO product_command (idempotency_key, canonical_command)
VALUES ($1::text, $2::bytea)
ON CONFLICT ON CONSTRAINT product_command_idempotency_key_pkey DO NOTHING
RETURNING
    product_id;
