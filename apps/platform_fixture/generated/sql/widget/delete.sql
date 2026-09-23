WITH target AS MATERIALIZED (
    SELECT id, edit_version
    FROM widget
    WHERE id = $1::uuid
    FOR UPDATE
),
deleted AS (
    DELETE FROM widget AS model
    USING target
    WHERE model.id = target.id
      AND target.edit_version = $2::int8
    RETURNING model.id
)
SELECT CASE
    WHEN NOT EXISTS (SELECT 1 FROM target) THEN 'not_found'
    WHEN NOT EXISTS (SELECT 1 FROM deleted) THEN 'concurrency_conflict'
    ELSE 'deleted'
END AS outcome;
