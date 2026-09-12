SELECT
    canonical_command,
    dock_id,
    finalized
FROM dock_create_command
WHERE idempotency_key = $1;
