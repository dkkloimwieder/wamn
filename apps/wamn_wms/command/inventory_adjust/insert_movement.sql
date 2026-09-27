INSERT INTO inventory_movement (
    pallet_id,
    product_id,
    kind,
    quantity,
    reason_code,
    occurred_at
)
VALUES ($1, $2, 'adjust', $3, $4, $5)
RETURNING id;
