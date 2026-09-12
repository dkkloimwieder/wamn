SELECT
    canonical_command,
    appointment_id,
    status
FROM appointment_book_command
WHERE idempotency_key = $1;
