INSERT INTO dock_create_command (
    idempotency_key,
    canonical_command
)
VALUES ($1, $2)
ON CONFLICT ON CONSTRAINT dock_create_command_idempotency_key_pkey
DO NOTHING
RETURNING dock_id;
