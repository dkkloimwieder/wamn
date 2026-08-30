WITH requested AS (
    SELECT
        purchase_order_line_id,
        quantity::numeric AS quantity,
        location_id
    FROM jsonb_to_recordset($2::jsonb) AS input (
        purchase_order_line_id uuid,
        quantity text,
        location_id uuid
    )
),
locked AS MATERIALIZED (
    SELECT
        purchase_order_line.id,
        purchase_order_line.purchase_order_id,
        purchase_order_line.ordered_quantity,
        purchase_order_line.received_quantity
    FROM purchase_order_line
    JOIN requested
        ON requested.purchase_order_line_id = purchase_order_line.id
    ORDER BY purchase_order_line.id
    FOR UPDATE OF purchase_order_line
),
locked_location AS MATERIALIZED (
    SELECT location.id
    FROM location
    WHERE location.id IN (
        SELECT requested.location_id
        FROM requested
    )
    ORDER BY location.id
    FOR KEY SHARE OF location
),
fact AS (
    SELECT
        requested.purchase_order_line_id,
        requested.quantity,
        locked.id AS locked_id,
        locked.purchase_order_id AS line_purchase_order_id,
        locked.ordered_quantity,
        locked.received_quantity,
        locked_location.id AS location_id
    FROM requested
    LEFT JOIN locked
        ON locked.id = requested.purchase_order_line_id
    LEFT JOIN locked_location
        ON locked_location.id = requested.location_id
)
SELECT CASE
    WHEN bool_or(locked_id IS NULL)
        THEN 'purchase_order_line_not_found'
    WHEN bool_or(line_purchase_order_id <> $1)
        THEN 'purchase_order_line_mismatch'
    WHEN bool_or(location_id IS NULL)
        THEN 'location_not_found'
    WHEN bool_or(quantity > ordered_quantity - received_quantity)
        THEN 'quantity_exceeds_remaining'
    ELSE 'ready'
END AS outcome
FROM fact;
