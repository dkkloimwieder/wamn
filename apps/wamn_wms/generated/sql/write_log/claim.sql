INSERT INTO app_system.write_log (operation, idempotency_key, request)
VALUES ($1::text, $2::text, $3::bytea)
ON CONFLICT (operation, idempotency_key) DO NOTHING
RETURNING idempotency_key;
