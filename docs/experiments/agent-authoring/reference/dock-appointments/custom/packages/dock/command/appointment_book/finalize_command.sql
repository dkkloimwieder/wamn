UPDATE appointment_book_command
SET status = $4
WHERE idempotency_key = $1
    AND canonical_command = $2
    AND appointment_id = $3
    AND status IS NULL
RETURNING status;
