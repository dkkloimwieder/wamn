SELECT
    canonical_command,
    receipt_id,
    purchase_order_id,
    purchase_order_status,
    row_version
FROM record_receipt_command
WHERE idempotency_key = $1;
