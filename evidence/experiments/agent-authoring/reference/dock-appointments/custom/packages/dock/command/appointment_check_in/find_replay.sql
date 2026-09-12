SELECT
    canonical_command,
    check_in_id,
    status,
    arrived_at
FROM appointment_check_in_command
WHERE idempotency_key = $1;
