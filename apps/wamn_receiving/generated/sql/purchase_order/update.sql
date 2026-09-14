WITH target AS MATERIALIZED (
    SELECT id, row_version
    FROM purchase_order
    WHERE id = $1::uuid
    FOR UPDATE
),
updated AS (
    UPDATE purchase_order AS model
    SET
        supplier_id = CASE WHEN $3::boolean THEN $4::uuid ELSE model.supplier_id END,
        row_version = CASE
            WHEN ($3::boolean AND $4::uuid::text IS DISTINCT FROM model.supplier_id::text)
            THEN model.row_version + 1
            ELSE model.row_version
        END
    FROM target
    WHERE model.id = target.id
      AND target.row_version = $2::int8
    RETURNING
    model.created_at,
    model.created_by,
    model.id,
    model.purchase_order_number,
    model.row_version,
    model.status,
    model.supplier_id,
    model.updated_at,
    model.updated_by
)
SELECT
    CASE
        WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'not_found'
        WHEN NOT EXISTS (SELECT 1 FROM updated) THEN 'concurrency_conflict'
        ELSE 'updated'
    END AS outcome,
    (SELECT target.row_version FROM target) AS observed_row_version,
    updated.created_at,
    updated.created_by,
    updated.id,
    updated.purchase_order_number,
    updated.row_version,
    updated.status,
    updated.supplier_id,
    updated.updated_at,
    updated.updated_by
FROM (SELECT 1) AS singleton
LEFT JOIN updated ON TRUE;
