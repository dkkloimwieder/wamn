WITH requested AS (
    SELECT
        purchase_order_line_id,
        quantity::numeric AS quantity,
        location_id,
        count(*) OVER (PARTITION BY purchase_order_line_id) AS line_count
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
        requested.line_count,
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
            WHEN line_count > 1
                THEN 'duplicate_line'
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
            WHEN line_count > 1
                THEN purchase_order_line_id
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
            WHEN 'duplicate_line' THEN 4
            WHEN 'quantity_exceeds_remaining' THEN 5
        END,
        id
    LIMIT 1
),
validation AS MATERIALIZED (
    SELECT
        COALESCE(violation.outcome, 'ready') AS outcome,
        violation.id
    FROM (SELECT 1) AS singleton
    LEFT JOIN violation ON TRUE
),
inserted_receipt AS (
    INSERT INTO receipt (purchase_order_id, receipt_reference, occurred_at)
    SELECT $1, $3, $4
    FROM validation
    WHERE validation.outcome = 'ready'
    RETURNING id
),
inserted_line AS (
    INSERT INTO receipt_line (receipt_id, purchase_order_line_id, quantity, location_id)
    SELECT inserted_receipt.id, requested.purchase_order_line_id, requested.quantity, requested.location_id
    FROM requested
    CROSS JOIN inserted_receipt
    RETURNING id
),
updated_line AS (
    UPDATE purchase_order_line
    SET received_quantity = purchase_order_line.received_quantity + requested.quantity
    FROM requested
    CROSS JOIN inserted_receipt
    CROSS JOIN (SELECT count(*) FROM inserted_line) AS written
    WHERE purchase_order_line.id = requested.purchase_order_line_id
        AND purchase_order_line.purchase_order_id = $1
        AND written.count = (SELECT count(*) FROM requested)
    RETURNING purchase_order_line.id, purchase_order_line.received_quantity
),
final_state AS (
    SELECT bool_and(
        COALESCE(updated_line.received_quantity, purchase_order_line.received_quantity)
            = purchase_order_line.ordered_quantity
    ) AS complete
    FROM purchase_order_line
    LEFT JOIN updated_line ON updated_line.id = purchase_order_line.id
    WHERE purchase_order_line.purchase_order_id = $1
),
finished AS (
    UPDATE purchase_order
    SET
        status = CASE WHEN COALESCE(final_state.complete, false) THEN 'complete' ELSE 'open' END,
        row_version = purchase_order.row_version + 1
    FROM final_state
    CROSS JOIN inserted_receipt
    WHERE purchase_order.id = $1
        AND purchase_order.status = 'open'
    RETURNING purchase_order.status, purchase_order.row_version
)
SELECT
    validation.outcome,
    validation.id,
    inserted_receipt.id AS receipt_id,
    finished.status,
    finished.row_version,
    (SELECT count(*) FROM inserted_line) AS inserted_line_count,
    (SELECT count(*) FROM updated_line) AS updated_line_count
FROM validation
LEFT JOIN inserted_receipt ON TRUE
LEFT JOIN finished ON TRUE;
