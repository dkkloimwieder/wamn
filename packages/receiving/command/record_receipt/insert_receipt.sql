INSERT INTO receipt (
    id,
    idempotency_key,
    purchase_order_id,
    receipt_reference,
    occurred_at
)
VALUES ($1, $2, $3, $4, $5)
RETURNING id;
