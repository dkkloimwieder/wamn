UPDATE appointment_check_in_command
SET
    status = $4,
    arrived_at = $5
WHERE idempotency_key = $1
    AND canonical_command = $2
    AND check_in_id = $3
    AND status IS NULL
    AND arrived_at IS NULL
RETURNING status, arrived_at;
