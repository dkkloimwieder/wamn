INSERT INTO supplier_command (idempotency_key, canonical_command)
VALUES ($1::text, $2::bytea)
ON CONFLICT ON CONSTRAINT supplier_command_idempotency_key_pkey DO NOTHING
RETURNING
    supplier_id;
