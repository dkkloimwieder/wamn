SELECT
    model.created_at,
    model.id,
    model.idempotency_key,
    model.occurred_at,
    model.purchase_order_id,
    model.receipt_reference
FROM receipt AS model
WHERE model.id = $1::uuid;
