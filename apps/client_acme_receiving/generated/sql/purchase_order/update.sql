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
        row_version = CASE
            WHEN ($3::boolean AND $4::boolean::text IS DISTINCT FROM model.acme_inspection_required::text)
            OR ($5::boolean AND $6::text::text IS DISTINCT FROM model.acme_quality_status::text)
            THEN model.row_version + 1
            ELSE model.row_version
        END
    FROM target
    WHERE model.id = target.id
      AND target.row_version = $2::int4
    RETURNING
    model.acme_inspection_required,
    model.acme_quality_status,
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
    updated.acme_inspection_required,
    updated.acme_quality_status,
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
