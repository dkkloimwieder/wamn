UPDATE dock_create_command
SET finalized = true
WHERE idempotency_key = $1
    AND canonical_command = $2
    AND dock_id = $3
    AND finalized IS NULL
RETURNING finalized;
