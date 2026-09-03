INSERT INTO inventory_movement (
    idempotency_key,
    pallet_id,
    product_id,
    kind,
    quantity,
    occurred_at
)
VALUES ($1, $2, $3, 'merge', $4, $5)
RETURNING id;
