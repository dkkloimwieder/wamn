-- The source lock is already held. Read the destination and balance afresh.
WITH held AS MATERIALIZED (
    SELECT id, quantity
    FROM packaging_quantity
    WHERE packaging_id = $1 AND product_id = $2 AND status = $3
),
outcome AS MATERIALIZED (
    SELECT CASE
        WHEN NOT EXISTS (SELECT 1 FROM location WHERE id = $7) THEN 'location_not_found'
        WHEN NOT EXISTS (SELECT 1 FROM held) THEN 'quantity_not_found'
        WHEN (SELECT quantity FROM held) <= $4 THEN 'insufficient_quantity'
        ELSE 'ok'
    END AS code
),
taken AS (
    UPDATE packaging_quantity AS quantity
    SET quantity = quantity.quantity - $4
    FROM held, outcome
    WHERE quantity.id = held.id AND outcome.code = 'ok'
    RETURNING quantity.id
),
created AS (
    INSERT INTO packaging (packaging_code, type, location_id, located_at, status)
    SELECT $5, $6, $7, $8, packaging.status
    FROM packaging, taken
    WHERE packaging.id = $1
    RETURNING id
),
placed AS (
    INSERT INTO packaging_quantity (packaging_id, product_id, status, quantity)
    SELECT created.id, $2, $3, $4 FROM created
    RETURNING packaging_id
),
written AS (
    INSERT INTO inventory_transaction (
        product_id, quantity, from_packaging_id, from_status,
        to_packaging_id, to_status, occurred_at
    )
    SELECT $2, $4, $1, $3, placed.packaging_id, $3, $8 FROM placed
    RETURNING id, to_packaging_id
),
touched AS (
    UPDATE packaging
    SET row_version = row_version + 1
    FROM written
    WHERE packaging.id = $1
    RETURNING packaging.row_version, packaging.status
)
SELECT outcome.code AS outcome,
    CASE outcome.code
        WHEN 'location_not_found' THEN jsonb_build_object('field', 'value.to_location_id', 'id', $7::text)
        WHEN 'quantity_not_found' THEN jsonb_build_object('field', 'value.product_id', 'id', $2::text)
        WHEN 'insufficient_quantity' THEN jsonb_build_object('field', 'value.quantity', 'observed', (SELECT quantity::text FROM held))
        ELSE '{}'::jsonb
    END AS detail,
    written.id AS transaction_id, written.to_packaging_id AS new_packaging_id,
    touched.row_version, touched.status
FROM outcome
LEFT JOIN written ON TRUE
LEFT JOIN touched ON TRUE;
