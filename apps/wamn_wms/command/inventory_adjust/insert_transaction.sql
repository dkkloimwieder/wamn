-- THE COUNT AGAINST THE BALANCE. A higher count is stock that appears, so the
-- row has only a to side; a lower count is stock that leaves, so the row has
-- only a from side. A count equal to the balance selects no row, and the
-- command refuses: an adjust that changes nothing writes nothing.
INSERT INTO inventory_transaction (
    product_id,
    quantity,
    from_packaging_id,
    from_status,
    to_packaging_id,
    to_status,
    reason_code,
    occurred_at
)
SELECT
    packaging_quantity.product_id,
    abs($4 - packaging_quantity.quantity),
    CASE WHEN $4 < packaging_quantity.quantity THEN packaging_quantity.packaging_id END,
    CASE WHEN $4 < packaging_quantity.quantity THEN packaging_quantity.status END,
    CASE WHEN $4 > packaging_quantity.quantity THEN packaging_quantity.packaging_id END,
    CASE WHEN $4 > packaging_quantity.quantity THEN packaging_quantity.status END,
    $5,
    $6
FROM packaging_quantity
WHERE packaging_quantity.packaging_id = $1
    AND packaging_quantity.product_id = $2
    AND packaging_quantity.status = $3
    AND packaging_quantity.quantity <> $4
RETURNING id;
