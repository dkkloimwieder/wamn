UPDATE app_system.write_log
SET result = $3::text
WHERE operation = $1::text
  AND idempotency_key = $2::text
  AND result IS NULL
RETURNING idempotency_key;
