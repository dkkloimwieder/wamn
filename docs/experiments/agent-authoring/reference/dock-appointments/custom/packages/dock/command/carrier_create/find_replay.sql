SELECT
    canonical_command,
    carrier_id,
    finalized
FROM carrier_create_command
WHERE idempotency_key = $1;
