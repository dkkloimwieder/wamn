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
        requested.location_id AS requested_location_id,
        locked_location.id AS locked_location_id
    FROM requested
    LEFT JOIN locked
        ON locked.id = requested.purchase_order_line_id
    LEFT JOIN locked_location
        ON locked_location.id = requested.location_id
),
classified AS (
    SELECT
        CASE
            WHEN locked_id IS NULL
                THEN 'purchase_order_line_not_found'
            WHEN line_purchase_order_id <> $1
                THEN 'purchase_order_line_mismatch'
            WHEN locked_location_id IS NULL
                THEN 'location_not_found'
            WHEN quantity > ordered_quantity - received_quantity
                THEN 'quantity_exceeds_remaining'
            ELSE NULL
        END AS outcome,
        CASE
            WHEN locked_id IS NULL
                THEN purchase_order_line_id
            WHEN line_purchase_order_id <> $1
                THEN purchase_order_line_id
            WHEN locked_location_id IS NULL
                THEN requested_location_id
            WHEN quantity > ordered_quantity - received_quantity
                THEN purchase_order_line_id
            ELSE NULL
        END AS id
    FROM fact
),
violation AS (
    SELECT outcome, id
    FROM classified
    WHERE outcome IS NOT NULL
    ORDER BY
        CASE outcome
            WHEN 'purchase_order_line_not_found' THEN 1
            WHEN 'purchase_order_line_mismatch' THEN 2
            WHEN 'location_not_found' THEN 3
            WHEN 'quantity_exceeds_remaining' THEN 4
        END,
        id
    LIMIT 1
)
SELECT
    COALESCE(violation.outcome, 'ready') AS outcome,
    violation.id
FROM (SELECT 1) AS singleton
LEFT JOIN violation ON TRUE;
