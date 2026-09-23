WITH target AS MATERIALIZED (
    SELECT id, edit_version
    FROM widget_tag
    WHERE id = $1::uuid
    FOR UPDATE
),
updated AS (
    UPDATE widget_tag AS model
    SET
        label = CASE WHEN $3::boolean THEN $4::text ELSE model.label END,
        edit_version = CASE
            WHEN ($3::boolean AND $4::text::text IS DISTINCT FROM model.label::text)
            THEN model.edit_version + 1
            ELSE model.edit_version
        END
    FROM target
    WHERE model.id = target.id
      AND target.edit_version = $2::int8
    RETURNING
    model.edit_version,
    model.id,
    model.label
)
SELECT
    CASE
        WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'not_found'
        WHEN NOT EXISTS (SELECT 1 FROM updated) THEN 'concurrency_conflict'
        ELSE 'updated'
    END AS outcome,
    (SELECT target.edit_version FROM target) AS observed_edit_version,
    updated.edit_version,
    updated.id,
    updated.label
FROM (SELECT 1) AS singleton
LEFT JOIN updated ON TRUE;
