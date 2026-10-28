-- Keep this statement separate: child quantities need a fresh snapshot after
-- any wait for the packaging lock under READ COMMITTED.
WITH locked AS MATERIALIZED (
    SELECT row_version, status
    FROM packaging
    WHERE id = $1
    FOR UPDATE
)
SELECT
    CASE
        WHEN NOT EXISTS (SELECT 1 FROM locked WHERE status <> 'consumed') THEN 'packaging_not_found'
        WHEN (SELECT row_version FROM locked) <> $2 THEN 'concurrency_conflict'
        ELSE 'ok'
    END AS outcome,
    CASE
        WHEN NOT EXISTS (SELECT 1 FROM locked WHERE status <> 'consumed')
            THEN jsonb_build_object('field', 'value.packaging_id', 'id', $1::text)
        WHEN (SELECT row_version FROM locked) <> $2
            THEN jsonb_build_object('expected_row_version', $2, 'observed_row_version', (SELECT row_version FROM locked))
        ELSE '{}'::jsonb
    END AS detail;
