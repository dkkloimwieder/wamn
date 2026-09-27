INSERT INTO inventory_transaction (
    product_id,
    quantity,
    from_packaging_id,
    from_status,
    to_packaging_id,
    to_status,
    occurred_at
)
VALUES ($1, $2, $3, $4, $5, $4, $6)
RETURNING id;
