INSERT INTO widget_command (idempotency_key, canonical_command)
VALUES ($1::text, $2::bytea)
ON CONFLICT ON CONSTRAINT widget_command_idempotency_key_pkey DO NOTHING
RETURNING
    widget_id;
