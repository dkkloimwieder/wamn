SELECT
    request,
    result
FROM app_system.write_log
WHERE operation = $1::text
  AND idempotency_key = $2::text;
