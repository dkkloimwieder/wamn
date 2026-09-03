INSERT INTO inventory_movement (
    idempotency_key,
    pallet_id,
    product_id,
    kind,
    from_location_id,
    to_location_id,
    quantity,
    occurred_at
)
VALUES ($1, $2, $3, 'move', $4, $5, $6, $7)
RETURNING id;
