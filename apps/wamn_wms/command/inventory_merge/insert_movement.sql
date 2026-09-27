INSERT INTO inventory_movement (
    pallet_id,
    product_id,
    kind,
    quantity,
    occurred_at
)
VALUES ($1, $2, 'merge', $3, $4)
RETURNING id;
