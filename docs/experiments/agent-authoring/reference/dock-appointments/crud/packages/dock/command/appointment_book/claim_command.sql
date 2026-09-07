INSERT INTO appointment_book_command (
    idempotency_key,
    canonical_command
)
VALUES ($1, $2)
ON CONFLICT ON CONSTRAINT appointment_book_command_idempotency_key_pkey
DO NOTHING
RETURNING appointment_id;
