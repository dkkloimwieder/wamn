WITH input AS (
    SELECT
        purchase_order_line_id,
        quantity::numeric AS quantity,
        location_id
    FROM jsonb_to_recordset($2::jsonb) AS item (
        purchase_order_line_id uuid,
        quantity text,
        location_id uuid
    )
)
INSERT INTO receipt_line (
    receipt_id,
    purchase_order_line_id,
    quantity,
    location_id
)
SELECT $1, purchase_order_line_id, quantity, location_id
FROM input
RETURNING id;
