WITH target AS MATERIALIZED (
    SELECT id, edit_version
    FROM widget
    WHERE id = $1::uuid
    FOR UPDATE
),
updated AS (
    UPDATE widget AS model
    SET
        code = CASE WHEN $3::boolean THEN $4::text ELSE model.code END,
        maker_id = CASE WHEN $5::boolean THEN $6::uuid ELSE model.maker_id END,
        note = CASE WHEN $7::boolean THEN $8::text ELSE model.note END,
        edit_version = CASE
            WHEN ($3::boolean AND $4::text::text IS DISTINCT FROM model.code::text)
            OR ($5::boolean AND $6::uuid::text IS DISTINCT FROM model.maker_id::text)
            OR ($7::boolean AND $8::text::text IS DISTINCT FROM model.note::text)
            THEN model.edit_version + 1
            ELSE model.edit_version
        END
    FROM target
    WHERE model.id = target.id
      AND target.edit_version = $2::int8
    RETURNING
    model.code,
    model.created_at,
    model.edit_version,
    model.id,
    model.maker_id,
    model.note
)
SELECT
    CASE
        WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'not_found'
        WHEN NOT EXISTS (SELECT 1 FROM updated) THEN 'concurrency_conflict'
        ELSE 'updated'
    END AS outcome,
    (SELECT target.edit_version FROM target) AS observed_edit_version,
    updated.code,
    updated.created_at,
    updated.edit_version,
    updated.id,
    updated.maker_id,
    updated.note
FROM (SELECT 1) AS singleton
LEFT JOIN updated ON TRUE;
