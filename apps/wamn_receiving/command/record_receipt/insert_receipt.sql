INSERT INTO receipt (
    purchase_order_id,
    receipt_reference,
    occurred_at
)
VALUES ($1, $2, $3)
RETURNING id;
