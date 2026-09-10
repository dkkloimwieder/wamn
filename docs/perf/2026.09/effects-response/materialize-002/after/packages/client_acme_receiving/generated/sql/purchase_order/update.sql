WITH target AS MATERIALIZED (
    SELECT id, row_version
    FROM purchase_order
    WHERE id = $1::uuid
    FOR UPDATE
),
updated AS (
    UPDATE purchase_order AS model
    SET
        acme_inspection_required = CASE WHEN $3::boolean THEN $4::boolean ELSE model.acme_inspection_required END,
        acme_quality_status = CASE WHEN $5::boolean THEN $6::text ELSE model.acme_quality_status END,
        row_version = model.row_version + 1
    FROM target
    WHERE model.id = target.id
      AND target.row_version = $2::int8
    RETURNING model.*
)
SELECT
    CASE
        WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'not_found'
        WHEN NOT EXISTS (SELECT 1 FROM updated) THEN 'concurrency_conflict'
        ELSE 'updated'
    END AS outcome,
    (SELECT target.row_version FROM target) AS observed_row_version,
    updated.acme_inspection_required,
    updated.acme_quality_status,
    updated.created_at,
    updated.id,
    updated.purchase_order_number,
    updated.row_version,
    updated.status,
    updated.supplier_id,
    updated.updated_at
FROM (SELECT 1) AS singleton
LEFT JOIN updated ON TRUE;
