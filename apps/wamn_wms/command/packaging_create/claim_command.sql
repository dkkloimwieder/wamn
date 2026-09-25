INSERT INTO packaging_create_command (idempotency_key, canonical_command) VALUES ($1, $2) ON CONFLICT (idempotency_key) DO NOTHING RETURNING operation_id, packaging_id;
