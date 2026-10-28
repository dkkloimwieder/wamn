-- The packaging lock is already held. This statement reads the fresh balance.
WITH held AS MATERIALIZED (
    SELECT id, quantity
    FROM packaging_quantity
    WHERE packaging_id = $1 AND product_id = $2 AND status = $3
),
outcome AS MATERIALIZED (
    SELECT CASE
        WHEN NOT EXISTS (SELECT 1 FROM held) THEN 'quantity_not_found'
        WHEN (SELECT quantity FROM held) = $4 THEN 'invalid_input'
        ELSE 'ok'
    END AS code
),
written AS (
    INSERT INTO inventory_transaction (
        product_id, quantity, from_packaging_id, from_status,
        to_packaging_id, to_status, reason_code, occurred_at
    )
    SELECT $2, abs($4 - held.quantity),
        CASE WHEN $4 < held.quantity THEN $1 END,
        CASE WHEN $4 < held.quantity THEN $3 END,
        CASE WHEN $4 > held.quantity THEN $1 END,
        CASE WHEN $4 > held.quantity THEN $3 END,
        $5, $6
    FROM held, outcome
    WHERE outcome.code = 'ok'
    RETURNING id
),
removed AS (
    DELETE FROM packaging_quantity AS quantity
    USING held, written
    WHERE quantity.id = held.id AND $4 = 0
    RETURNING quantity.quantity
),
changed AS (
    UPDATE packaging_quantity AS quantity
    SET quantity = $4
    FROM held, written
    WHERE quantity.id = held.id AND $4 <> 0
    RETURNING quantity.quantity
),
touched AS (
    UPDATE packaging
    SET row_version = row_version + 1
    WHERE id = $1 AND EXISTS (SELECT 1 FROM removed UNION ALL SELECT 1 FROM changed)
    RETURNING row_version, status
)
SELECT outcome.code AS outcome,
    CASE outcome.code
        WHEN 'quantity_not_found' THEN jsonb_build_object('field', 'value.product_id', 'id', $2::text)
        WHEN 'invalid_input' THEN jsonb_build_object('field', 'value.quantity')
        ELSE '{}'::jsonb
    END AS detail,
    written.id AS transaction_id,
    CASE WHEN EXISTS (SELECT 1 FROM removed) THEN $4 ELSE (SELECT quantity FROM changed) END AS adjusted_quantity,
    touched.row_version, touched.status
FROM outcome
LEFT JOIN written ON TRUE
LEFT JOIN touched ON TRUE;
