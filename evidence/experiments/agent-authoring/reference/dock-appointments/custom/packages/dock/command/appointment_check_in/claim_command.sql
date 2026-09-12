INSERT INTO appointment_check_in_command (
    idempotency_key,
    canonical_command
)
VALUES ($1, $2)
ON CONFLICT ON CONSTRAINT appointment_check_in_command_idempotency_key_pkey
DO NOTHING
RETURNING check_in_id;
