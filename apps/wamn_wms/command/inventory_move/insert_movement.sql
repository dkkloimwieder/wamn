INSERT INTO inventory_movement (
    pallet_id,
    product_id,
    kind,
    from_location_id,
    to_location_id,
    quantity,
    occurred_at
)
VALUES ($1, $2, 'move', $3, $4, $5, $6)
RETURNING id;
