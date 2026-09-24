WITH target AS MATERIALIZED (
    SELECT id, row_version
    FROM product
    WHERE id = $1::uuid
    FOR UPDATE
),
updated AS (
    UPDATE product AS model
    SET
        product_code = CASE WHEN $3::boolean THEN $4::text ELSE model.product_code END,
        row_version = CASE
            WHEN ($3::boolean AND $4::text::text IS DISTINCT FROM model.product_code::text)
            THEN model.row_version + 1
            ELSE model.row_version
        END
    FROM target
    WHERE model.id = target.id
      AND target.row_version = $2::int4
    RETURNING
    model.created_at,
    model.id,
    model.product_code,
    model.row_version
)
SELECT
    CASE
        WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'not_found'
        WHEN NOT EXISTS (SELECT 1 FROM updated) THEN 'concurrency_conflict'
        ELSE 'updated'
    END AS outcome,
    (SELECT target.row_version FROM target) AS observed_row_version,
    updated.created_at,
    updated.id,
    updated.product_code,
    updated.row_version
FROM (SELECT 1) AS singleton
LEFT JOIN updated ON TRUE;
