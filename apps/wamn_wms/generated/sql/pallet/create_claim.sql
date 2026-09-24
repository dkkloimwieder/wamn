INSERT INTO pallet_command (idempotency_key, canonical_command)
VALUES ($1::text, $2::bytea)
ON CONFLICT ON CONSTRAINT pallet_command_idempotency_key_pkey DO NOTHING
RETURNING
    pallet_id;
