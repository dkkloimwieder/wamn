UPDATE record_receipt_command
SET
    purchase_order_status = $4,
    row_version = $5
WHERE idempotency_key = $1
    AND canonical_command = $2
    AND receipt_id = $3
    AND purchase_order_status IS NULL
    AND row_version IS NULL
RETURNING purchase_order_status, row_version;
