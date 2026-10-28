-- Both packaging locks are already held. Read source rows with a fresh snapshot.
WITH source AS MATERIALIZED (
    SELECT id, product_id, quantity, status
    FROM packaging_quantity
    WHERE packaging_id = $1
),
placed AS (
    INSERT INTO packaging_quantity (packaging_id, product_id, status, quantity)
    SELECT $2, product_id, status, quantity FROM source
    ORDER BY product_id ASC, status ASC
    ON CONFLICT ON CONSTRAINT packaging_quantity_packaging_id_product_id_status_key
    DO UPDATE SET quantity = packaging_quantity.quantity + EXCLUDED.quantity
    RETURNING product_id, status
),
written AS (
    INSERT INTO inventory_transaction (
        product_id, quantity, from_packaging_id, from_status,
        to_packaging_id, to_status, occurred_at
    )
    SELECT source.product_id, source.quantity, $1, source.status, $2, source.status, $3
    FROM source
    JOIN placed USING (product_id, status)
    RETURNING id, product_id, from_status
),
removed AS (
    DELETE FROM packaging_quantity AS quantity
    USING source, written
    WHERE quantity.id = source.id
        AND source.product_id = written.product_id AND source.status = written.from_status
    RETURNING quantity.id
),
consumed AS (
    UPDATE packaging
    SET status = 'consumed', row_version = row_version + 1
    WHERE id = $1 AND EXISTS (SELECT 1 FROM removed)
    RETURNING id
),
touched AS (
    UPDATE packaging
    SET row_version = row_version + 1
    WHERE id = $2 AND EXISTS (SELECT 1 FROM consumed)
    RETURNING row_version, status
)
SELECT
    CASE WHEN EXISTS (SELECT 1 FROM source) THEN 'ok' ELSE 'invalid_input' END AS outcome,
    CASE WHEN EXISTS (SELECT 1 FROM source) THEN '{}'::jsonb
        ELSE jsonb_build_object('field', 'value.source_packaging_id') END AS detail,
    (SELECT coalesce(jsonb_agg(id ORDER BY product_id ASC, from_status ASC), '[]'::jsonb) FROM written) AS transaction_ids,
    touched.row_version, touched.status
FROM (SELECT 1) AS singleton
LEFT JOIN touched ON TRUE;
