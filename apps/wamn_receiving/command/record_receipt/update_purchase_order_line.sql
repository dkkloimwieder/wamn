WITH input AS (
    SELECT
        purchase_order_line_id,
        quantity::numeric AS quantity
    FROM jsonb_to_recordset($2::jsonb) AS item (
        purchase_order_line_id uuid,
        quantity text,
        location_id uuid
    )
)
UPDATE purchase_order_line
SET received_quantity = purchase_order_line.received_quantity + input.quantity
FROM input
WHERE purchase_order_line.id = input.purchase_order_line_id
    AND purchase_order_line.purchase_order_id = $1
RETURNING purchase_order_line.id;
