WITH locked_inspection AS MATERIALIZED (
    SELECT
        quality_inspection.receipt_id,
        quality_inspection.status,
        quality_inspection.row_version,
        receipt.purchase_order_id
    FROM quality_inspection
    JOIN receipt
        ON receipt.id = quality_inspection.receipt_id
    WHERE quality_inspection.receipt_id = $1
    FOR UPDATE OF quality_inspection
),
approved_inspection AS (
    UPDATE quality_inspection
    SET
        status = 'approved',
        row_version = quality_inspection.row_version + 1
    FROM locked_inspection
    WHERE quality_inspection.receipt_id = locked_inspection.receipt_id
        AND locked_inspection.status = 'pending'
        AND locked_inspection.row_version = $2
    RETURNING
        quality_inspection.receipt_id,
        quality_inspection.status,
        quality_inspection.row_version,
        locked_inspection.purchase_order_id
),
approved_purchase_order AS (
    UPDATE purchase_order
    SET
        acme_quality_status = 'approved',
        row_version = purchase_order.row_version + 1
    FROM approved_inspection
    WHERE purchase_order.id = approved_inspection.purchase_order_id
    RETURNING
        purchase_order.id AS purchase_order_id,
        purchase_order.row_version AS purchase_order_row_version
),
successful AS (
    SELECT
        approved_inspection.receipt_id,
        approved_inspection.status,
        approved_inspection.row_version,
        approved_purchase_order.purchase_order_id,
        approved_purchase_order.purchase_order_row_version
    FROM approved_inspection
    JOIN approved_purchase_order
        ON approved_purchase_order.purchase_order_id =
            approved_inspection.purchase_order_id

    UNION ALL

    SELECT
        locked_inspection.receipt_id,
        locked_inspection.status,
        locked_inspection.row_version,
        purchase_order.id AS purchase_order_id,
        purchase_order.row_version AS purchase_order_row_version
    FROM locked_inspection
    JOIN purchase_order
        ON purchase_order.id = locked_inspection.purchase_order_id
    WHERE locked_inspection.status = 'approved'
        AND locked_inspection.row_version = $2
)
SELECT
    CASE
        WHEN locked_inspection.receipt_id IS NULL THEN 'not_found'
        WHEN locked_inspection.row_version <> $2 THEN 'concurrency_conflict'
        ELSE 'approved'
    END AS outcome,
    locked_inspection.row_version AS observed_row_version,
    successful.receipt_id,
    successful.status,
    successful.row_version,
    successful.purchase_order_id,
    successful.purchase_order_row_version
FROM (SELECT 1) AS singleton
LEFT JOIN locked_inspection ON TRUE
LEFT JOIN successful
    ON successful.receipt_id = locked_inspection.receipt_id;
