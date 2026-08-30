INSERT INTO record_receipt_command (
    idempotency_key,
    canonical_command,
    purchase_order_id
)
VALUES ($1, $2, $3)
ON CONFLICT ON CONSTRAINT record_receipt_command_idempotency_key_pkey
DO NOTHING
RETURNING receipt_id;
