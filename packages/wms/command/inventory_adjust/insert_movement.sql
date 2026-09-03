INSERT INTO inventory_movement (
    idempotency_key,
    pallet_id,
    product_id,
    kind,
    quantity,
    reason_code,
    occurred_at
)
VALUES ($1, $2, $3, 'adjust', $4, $5, $6)
RETURNING id;
