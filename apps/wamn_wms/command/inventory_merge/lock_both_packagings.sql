-- Lock in id order so opposite merges cannot deadlock. The next statement
-- reads child quantities with a fresh snapshot after any lock wait.
WITH locked AS MATERIALIZED (
    SELECT id, row_version, status
    FROM packaging
    WHERE id IN ($1, $2)
    ORDER BY id ASC
    FOR UPDATE
),
outcome AS (
    SELECT CASE
        WHEN NOT EXISTS (SELECT 1 FROM locked WHERE id = $1 AND status <> 'consumed') THEN 'source_not_found'
        WHEN NOT EXISTS (SELECT 1 FROM locked WHERE id = $2 AND status <> 'consumed') THEN 'target_not_found'
        WHEN (SELECT row_version FROM locked WHERE id = $2) <> $3 THEN 'concurrency_conflict'
        ELSE 'ok'
    END AS code
)
SELECT
    CASE WHEN code IN ('source_not_found', 'target_not_found') THEN 'packaging_not_found' ELSE code END AS outcome,
    CASE code
        WHEN 'source_not_found' THEN jsonb_build_object('field', 'value.source_packaging_id', 'id', $1::text)
        WHEN 'target_not_found' THEN jsonb_build_object('field', 'value.target_packaging_id', 'id', $2::text)
        WHEN 'concurrency_conflict' THEN jsonb_build_object('expected_row_version', $3, 'observed_row_version', (SELECT row_version FROM locked WHERE id = $2))
        ELSE '{}'::jsonb
    END AS detail
FROM outcome;
