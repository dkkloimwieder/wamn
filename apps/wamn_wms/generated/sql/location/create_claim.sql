INSERT INTO location_command (idempotency_key, canonical_command)
VALUES ($1::text, $2::bytea)
ON CONFLICT ON CONSTRAINT location_command_idempotency_key_pkey DO NOTHING
RETURNING
    location_id;
